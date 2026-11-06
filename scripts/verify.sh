#!/usr/bin/env bash
# The one command that proves this tree builds, that its code passes, and that the artifact it
# ships serves (ADR-0003).
#
# Usage: scripts/verify.sh [--keep] [--no-build]
# Seven phases, cheapest first: cargo fmt --check, cargo clippy --all-targets -- -D warnings,
# cargo test, `cargo test --test schema_previous` (the expand/contract rule, HS-075),
# scripts/build.sh (which runs scripts/build-web.sh and scripts/check-static.sh),
# scripts/check-embed.sh, then serve: dist/<host-triple>/holosmith on a random port with a temp
# config and a temp data dir, answering /api/health, the schema version, the app shell, the 404
# envelope, every browser-facing guard refusal (ADR-0010), the create endpoint's 202/422/409
# answers, the sessions list, one session by id, the session screen's path, the session's event
# stream, a prompt's turn, a turn's deltas coalesced into no more rows than the script's frames, a
# timeout that does not fire on a short turn, an abort down to
# `abort_completed`, a steer's own frame, a resume's two
# refusals, a delete from its 202 to the row that is gone, an idle session stopped by the sweeper with
# its reason, its event and its log line, a create at the cap evicting the idle session before it
# succeeds, the expected-stop flag an eviction records and a resume clears, a token budget on a boot of
# its own ending the turn that spends it, a worktree made in a real
# repository and taken back when the next step refuses, the three project refusals a create and a resume
# answer with their own codes, the Internet-access checkbox measured on two real session networks (nothing
# reachable directly, the proxy's alias resolvable in exactly one of them, and the flag as a network's endpoint
# count after a resume), a `kill -9` of the server mid-turn and a restart (the row comes back idle with
# `interrupted`, the seqs stay unique and the counter is the log's highest), the names a crash leaves behind
# swept on a real daemon (a leftover container, an orphan network and worktree directory gone, a kept branch
# and ref kept, the session's container replaced under the same name), a boot over a database from a newer
# release (which continues and says who refuses), the verification table itself (no V1-V10 row still
# says `not yet run`, every row carries its verdict and an automation this repository has, and V11 is
# still deferred to phase 3), and a Docker preflight that
# refuses to start with one sentence naming the fix, with no CORS header
# on any answer, and stopping on SIGTERM with exit 0. Each case prints `ok <case>` once it passes — a
# case that printed SKIP withholds that line, so the log can never call a check that measured nothing a
# pass — which makes the run's own output say which check each verdict in that table rests on (HS-046).
# --no-build skips the two artifact phases; --keep keeps the work dir of a successful run. On
# failure this prints FAIL <assertion> and keeps the work dir (config, banner, every answer).
# Downloads nothing and needs neither curl nor jq: every request is a /dev/tcp fetch. `case_worktree`
# answers the Docker preflight from a python3 socket, and prints one SKIP line and passes without
# python3 (tests/worktree.rs covers what that case asserts). Put the
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
socket_pid=""
holder_pid=""
serve_err="$WORK/serve.err"
created_id=""
# The preview listener's bound port, read from the banner (HS-041): the config asks for port 0.
preview_listen=""
preview_port=""
assertions=0

# The only cleanup path: the work dir is removed on success, kept on failure (and with --keep),
# and a server a failure left running is stopped here rather than outliving the run.
# The session the running Docker case made, if any: the cases that start a real container set this, so a `fail`
# in the middle of one does not leave a live container and network on the machine (HS-031's review — the EXIT
# trap is the only hook a `fail` leaves, because `fail` exits the script). A case that makes **two** sessions
# (HS-038's) sets the second one too: one id cannot describe a pair, and a half-cleaned failure leaves exactly
# the orphan the trap exists to prevent.
docker_session_id=""
docker_second_session_id=""

cleanup() {
  local status=$?
  if [ -n "$serve_pid" ] && kill -0 "$serve_pid" 2>/dev/null; then
    kill -TERM "$serve_pid" 2>/dev/null || true
    wait "$serve_pid" 2>/dev/null || true
  fi
  if [ -n "$socket_pid" ] && kill -0 "$socket_pid" 2>/dev/null; then
    kill "$socket_pid" 2>/dev/null || true
    wait "$socket_pid" 2>/dev/null || true
  fi
  # A case that holds a port on purpose (HS-045's `doctor`, whose `ports` check is the one thing it can break
  # without touching the machine) leaves its holder here, so a `fail` in the middle does not strand a listener.
  if [ -n "$holder_pid" ] && kill -0 "$holder_pid" 2>/dev/null; then
    kill "$holder_pid" 2>/dev/null || true
    wait "$holder_pid" 2>/dev/null || true
  fi
  local id
  for id in ${docker_session_id:-} ${docker_second_session_id:-}; do
    docker rm --force "s_$id" >/dev/null 2>&1 || true
    # The egress proxy is an endpoint on a session network that allowed internet access, and `docker network rm`
    # refuses a network with one — so a teardown that forgot the detach would leave the network behind with a
    # `warn`, which is the orphan HS-044 owns. `docker.rs`'s `remove_network` leaves first for the same reason.
    docker network disconnect --force "agents_$id" hs_egress >/dev/null 2>&1 || true
    docker network rm "agents_$id" >/dev/null 2>&1 || true
  done
  docker_session_id=""
  docker_second_session_id=""
  # shellcheck disable=SC2086 # deliberate word splitting: each id is one word, and an empty list is no words.
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

# The same `/dev/tcp` fetch as `fetch`, against the **preview** listener rather than the control one (HS-041).
#
# A second function rather than a port parameter, for the reason the script already has three fetch shapes: every
# other case quotes its answers under a control label, and a preview answer saved under one of those would make
# the CORS sweep and the assertion names lie about which listener said what.
preview_fetch() {
  local label="$1" method="$2" path="$3"
  shift 3
  local -a sent=("$@")
  local request="$method $path HTTP/1.1"$'\r\n' fd header
  for header in "${sent[@]}"; do
    request+="$header"$'\r\n'
  done
  request+=$'Connection: close\r\n\r\n'
  exec {fd}<>"/dev/tcp/127.0.0.1/${preview_port}" || return 1
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
# session's log from its beginning — the shared boot keeps the default `history_window`, so nothing it
# reads is truncated (HS-061) — and the log grows with every turn this run takes: by the abort and steer
# cases the shared session has had four or five turns, which is 300-odd lines, so a budget sized for the
# log as it was at HS-019 would end the read before the frame the case came for. A stream that has not
# settled after 200 events would be a bug of its own, and 200 events is what 800 lines is.
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
  case_verification_table
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
  case_event_coalescing
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
  case_tool_output
  case_tool_image
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
  case_delete_keeps
  case_idle_stop
  case_delete_unknown_session
  case_create_at_cap_evicts
  case_exit_classification
  case_create_at_cap_refused
  case_token_budget
  case_usage_survives_resume
  case_compaction
  case_history
  case_backup
  case_worktree
  case_docker_preflight
  case_doctor_quick
  case_server_deploy
  case_worktree_refusal
  case_container_mounts
  case_agent_launch
  case_docker_resume
  case_docker_turn
  case_delete_keeps_the_work
  case_reconcile
  case_reconcile_orphans
  case_network_selftest
  case_key_proxy
  case_key_proxy_request
  case_snapshot
  case_egress_proxy
  case_internet_toggle
  case_preview
  case_schema_newer
  case_doctor_full
)

# HS-046: the spec's verification table is the record of phase 1's verification run, and this is what keeps
# it honest after the story that filled it. It reads `HolosmithSpec.md`'s eleven V rows and asserts four things:
# no V1-V10 row still says "not yet run"; each carries a verdict (`**pass**` or `**fail**`, the words the
# table's own column uses); each names an automation this repository really has — a `case_*` in serve_cases,
# a `tests/*.rs` under the crate, a `scripts/*.sh`, or the by-hand procedure in `docs/build.md` — and V11 still
# says it is deferred to phase 3 (HS-070), because that is the one row a phase-2 story may not fill.
#
# It is deliberately not a second copy of the table: it reads every `V<n>` row out of the spec whatever its
# number, so a row a later story adds is measured by the same rules. The eleven-row count is what keeps that
# honest in both directions — a broken read (a renamed heading, a table that moved out of the file) fails
# instead of passing over nothing, and adding a twelfth row is a deliberate edit here rather than a row the
# case walks past. A row that says `fail` must also name the fallback it took, because acceptance bullet 2
# asks for the fallback and not only the verdict. The mapping it enforces is the story's own deliverable: a
# cell names the case that filled it, and "every row is automated" stops being prose the moment a case is
# renamed (ADR-0055).
#
# It needs no server, no daemon and no image, which is why it runs first in serve_cases: a table that has
# rotted fails the serve phase before any Docker case, rather than after ten minutes of them.
case_verification_table() {
  local name=case_verification_table
  local spec="$repo_root/HolosmithSpec.md"
  local rows row_count gate_cases n row cell token tokens
  [ -f "$spec" ] || fail "$name/spec" "$spec is not there: the table is phase 1's record"
  # Every `| V<n> |` row, whatever its number: the count below is what makes adding a row a deliberate edit
  # here instead of a row nothing reads.
  rows="$(grep -E '^\| V[0-9][0-9]* \|' "$spec" || true)"
  row_count="$(printf '%s\n' "$rows" | grep -c . || true)"
  check "$name/rows" 11 "$row_count"
  # The cases this gate really runs, so a cell naming one that has been renamed or dropped fails here instead
  # of quoting a check nobody runs any more.
  gate_cases="$(printf '%s\n' "${serve_cases[@]}")"
  for n in $(printf '%s\n' "$rows" | sed -n 's/^| V\([0-9][0-9]*\) |.*/\1/p'); do
    row="$(printf '%s\n' "$rows" | grep -E "^\| V$n \|" || true)"
    [ -n "$row" ] || fail "$name/V$n" "no V$n row in $spec"
    # The Result cell is the row's last field: strip the closing pipe, then everything up to the last one.
    cell="$(printf '%s\n' "$row" | sed -e 's/[[:space:]]*|[[:space:]]*$//' -e 's/.*|[[:space:]]*//')"
    [ -n "$cell" ] || fail "$name/V$n" "V$n's Result cell is empty"
    if printf '%s\n' "$cell" | grep -qi 'not yet run'; then
      fail "$name/V$n" "V$n still says \"not yet run\": $cell"
    fi
    if [ "$n" = 11 ]; then
      printf '%s\n' "$cell" | grep -q 'deferred to phase 3' ||
        fail "$name/V11" "V11 no longer says it is deferred to phase 3: $cell"
      printf '%s\n' "$cell" | grep -q 'HS-070' ||
        fail "$name/V11" "V11's deferral names no story: $cell"
      assertions=$((assertions + 2))
      continue
    fi
    if printf '%s\n' "$cell" | grep -q '\*\*fail'; then
      # Acceptance bullet 2: a failure records which fallback was taken, not only that it failed.
      printf '%s\n' "$cell" | grep -qi 'fallback' ||
        fail "$name/V$n" "V$n says fail and records no fallback: $cell"
      assertions=$((assertions + 1))
    else
      printf '%s\n' "$cell" | grep -q '\*\*pass' ||
        fail "$name/V$n" "V$n carries no verdict (**pass** or **fail**): $cell"
    fi
    assertions=$((assertions + 1))
    tokens="$(printf '%s\n' "$cell" |
      grep -o -E 'case_[a-z0-9_]+|tests/[a-z0-9_]+\.rs|scripts/[a-z0-9-]+\.sh|docs/build\.md' || true)"
    [ -n "$tokens" ] ||
      fail "$name/V$n" "V$n names no automation (a case_*, a tests/*.rs, a scripts/*.sh or docs/build.md): $cell"
    for token in $tokens; do
      case "$token" in
        case_*)
          printf '%s\n' "$gate_cases" | grep -qx "$token" ||
            fail "$name/V$n" "V$n names $token, which is not in serve_cases"
          ;;
        tests/*)
          [ -f "$repo_root/crates/holosmith/$token" ] ||
            fail "$name/V$n" "V$n names $token, which is not under crates/holosmith"
          ;;
        *)
          [ -f "$repo_root/$token" ] ||
            fail "$name/V$n" "V$n names $token, which does not exist"
          ;;
      esac
      assertions=$((assertions + 1))
    done
  done
}

# V1 (HS-027): the hardened `.git` mounts, measured on a real daemon. Three writes must fail, three git
# commands must work, the process must not be root and the container must not be able to remount or
# unmount. This case needs no server: it is a fact about `docker run` and the mounts, so it never touches
# the booted artifact — `tests/container.rs` asserts that the arguments the binary builds are these.
#
# Two environment facts, both measured (ADR-0035): the daemon resolves host paths in the *host's* mount
# namespace, so the scratch repository lives under `target/` rather than in `$WORK` (`mktemp -d` is on
# `/tmp`, which a sandbox can put on a tmpfs the daemon cannot see, and `-v` then silently creates an
# empty root-owned directory instead of mounting anything); and Docker's own English is never parsed — the
# case reads exit codes and the host's files.
case_container_mounts() {
  local name=case_container_mounts
  local id=0123456789abcdef image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local root="$repo_root/target/verify-container-$$" projects="$repo_root/target/verify-container-$$/projects"
  local project="$repo_root/target/verify-container-$$/projects/holosmith"
  local git_dir hooks wrote
  # The guards come first: a machine without a daemon is a supported machine (a hub), and this case is
  # the shipped gate's share of V1 only because it is cheap here.
  if ! command -v docker >/dev/null 2>&1; then
    printf 'SKIP %s: no docker CLI (tests/container.rs is the same check)\n' "$name"
    return 0
  fi
  if ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/container.rs is the same check)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi

  rm -rf "$root"
  mkdir -p "$project" || fail "$name/mkdir" "could not create $project"
  git -C "$project" init --quiet -b main . || fail "$name/git" "git init did not work in $project"
  git -C "$project" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work in $project"
  git -C "$project" worktree add -b "agent/$id" "$projects/.worktrees/$id" HEAD >/dev/null \
    2>&1 || fail "$name/worktree" "git worktree add did not work"
  git_dir="$project/.git"
  hooks="$git_dir/hooks"
  cp "$git_dir/config" "$WORK/config-before"

  # The four mounts of spec item 1, spelled the way the spec spells them.
  docker rm --force "s_$id" >/dev/null 2>&1 || true
  wrote="$(docker run --name "s_$id" --cap-drop ALL --security-opt no-new-privileges \
    --workdir /work --env "HS_SESSION_ID=$id" \
    -v "$projects/.worktrees/$id":/work \
    -v "$git_dir":"$git_dir" \
    -v "$hooks":"$hooks":ro \
    -v "$git_dir/config":"$git_dir/config":ro \
    "$image" sh -c "
      id -u 2>/dev/null || echo missing
      ( echo x > $hooks/pre-commit ) >/dev/null 2>&1; echo hooks_write=\$?
      ( echo '# x' >> $git_dir/config ) >/dev/null 2>&1; echo config_append=\$?
      ( mv $hooks $hooks.renamed ) >/dev/null 2>&1; echo hooks_rename=\$?
      ( echo two > second.txt && git add second.txt &&
        git -c user.email=verify@example -c user.name=verify commit --quiet -m second ) \
        >/dev/null 2>&1; echo commit=\$?
      git branch agent/v1-probe >/dev/null 2>&1; echo branch=\$?
      git worktree list; echo worktree_list=\$?
      command -v mount >/dev/null 2>&1 && command -v umount >/dev/null 2>&1 \
        && echo tools=present || echo tools=missing
      mount -o remount,rw / >/dev/null 2>&1; echo remount=\$?
      umount /work >/dev/null 2>&1; echo umount=\$?
    " 2>"$WORK/responses/$name.err")" || fail "$name/docker_run" "docker run failed: $(cat "$WORK/responses/$name.err")"
  docker rm --force "s_$id" >/dev/null 2>&1 || true

  # 127 is "no such file": a missing tool is not a refused mount, so it fails rather than passing for the
  # wrong reason (the review of this story's test found the same hole).
  printf '%s\n' "$wrote" | grep -qx 'tools=present' ||
    fail "$name/tools" "the image has no mount/umount, so that clause cannot be measured: $wrote"
  assertions=$((assertions + 1))
  local refused
  for refused in hooks_write config_append hooks_rename remount umount; do
    case "$wrote" in
      *"$refused=0"*) fail "$name/$refused" "the container was allowed to do it: $wrote" ;;
      *"$refused=127"*) fail "$name/$refused" "not a program in this image: $wrote" ;;
      *"$refused="*) assertions=$((assertions + 1)) ;;
      *) fail "$name/$refused" "the container reported no $refused: $wrote" ;;
    esac
  done
  local worked
  for worked in commit branch worktree_list; do
    case "$wrote" in
      *"$worked=0"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/$worked" "git $worked failed inside the container: $wrote" ;;
    esac
  done
  # The host is where a refusal is observable: no hook arrived, the config is unchanged and the hooks
  # directory was not moved.
  [ ! -e "$hooks/pre-commit" ] || fail "$name/hook_reached_host" "a hook reached the host"
  assertions=$((assertions + 1))
  [ -d "$hooks" ] || fail "$name/hooks_moved" "the hooks directory was moved out from under the host"
  assertions=$((assertions + 1))
  cmp -s "$WORK/config-before" "$git_dir/config" ||
    fail "$name/config_changed" "the repository's config was rewritten through a read-only mount"
  assertions=$((assertions + 1))
  # The commit is in the host's repository, on the session's branch.
  git -C "$project" log --format=%s "agent/$id" | grep -q second ||
    fail "$name/commit_lost" "the container's commit is not on agent/$id"
  assertions=$((assertions + 1))
  git -C "$project" branch --list agent/v1-probe | grep -q agent/v1-probe ||
    fail "$name/branch_lost" "the branch the container created is not in the host's repository"
  assertions=$((assertions + 1))
  # Not root: the image's own user runs the agent, and the uid is the first line the container printed.
  local uid
  uid="$(printf '%s\n' "$wrote" | sed -n '1p')"
  case "$uid" in
    '' | *[!0-9]*) fail "$name/uid" "the container reported no numeric uid, so \"not root\" proves nothing: $wrote" ;;
  esac
  [ "$uid" != "0" ] || fail "$name/root" "the agent runs as root: $wrote"
  assertions=$((assertions + 1))

  rm -rf "$root"
}

# HS-028's proof from the artifact: a Docker boot runs the session's network and its container, and the row
# therefore ends `error` at the **agent** step rather than at the network or the container one.
#
# The container's own flags are `tests/container.rs`'s (it inspects a real container, which the API cannot
# show: the provisioner takes the container back the moment the agent step fails, exactly as ADR-0015 says).
# What this case proves is the *order* — two steps that go to a real daemon succeeded — and the teardown: no
# container, no network and no worktree are left behind, and the registration list is back to one entry.
#
# Its projects root lives under `target/` because the daemon resolves bind-mount sources in the host's
# namespace and `$WORK` is on `/tmp` (ADR-0035's measurement).
# HS-031's proof from the artifact: a Docker boot runs the session's network, its container **and the
# agent**, so a create reaches `idle` and the container is pi in RPC mode with the session's own agent
# directory mounted into it. Then the container is removed from under the server, and the exit the attached
# stdio observed is classified: the row is `error` with the last of what pi printed, exactly spec item 6's
# third row.
#
# The container's flags are `tests/container.rs`'s (the API cannot show them, and `docker inspect` is the only
# place a running container's real configuration is visible). What this case adds is the *artifact's* half:
# the four provisioning steps end in `idle`, the launch line and the seed are on disk where the data root
# says, and the delete takes all of it away.
#
# Its projects root **and its data root** live under `target/` because the daemon resolves bind-mount
# sources in the host's namespace and `$WORK` is on `/tmp` (ADR-0035): since HS-031 a data root the daemon
# cannot see would mount an empty agent directory and fail the handshake, silently.
case_agent_launch() {
  local name=case_agent_launch body id image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local root="$repo_root/target/verify-agent-$$" projects="$repo_root/target/verify-agent-$$/projects"
  local repo="$repo_root/target/verify-agent-$$/projects/holosmith"
  local data="$repo_root/target/verify-agent-$$/data"
  local home log
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/agent.rs is the same check)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$repo" || fail "$name/mkdir" "could not create $repo"
  git -C "$repo" init --quiet . || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  # The image is a key, and so is every limit, so the boot proves the config carries them into a real `docker
  # run`. `idle_stop_min = 0` because this boot lasts seconds and a sweeper would only be noise.
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\nagent_image = "%s"\ncontainer_cpus = 1\ncontainer_memory_mb = 512\ncontainer_pids = 128\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$image" >"$WORK/config-agent.toml"
  start_serve "$WORK/config-agent.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-agent-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  docker_session_id="$id"

  # The row is the wait: four steps against a real daemon, the last of them a container start plus pi's
  # handshake. `idle` is the story's demo — pi answered, so the session can take a prompt.
  body=""
  for _ in $(seq 1 900); do
    fetch "${name}_row" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  expect_status "${name}_row" "$(answer "${name}_row")" 200
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "${name}_row/status" "the session did not become idle: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF "\"worktree\":\"$projects/.worktrees/$id\"" ||
    fail "${name}_row/worktree" "the row does not record the worktree path: $body"
  assertions=$((assertions + 1))

  # The agent's own directory, seeded from the template with the row's two preferences (spec item 2).
  home="$data/sessions/$id/pi-home"
  log="$data/sessions/$id/session.log"
  [ -f "$home/settings.json" ] ||
    fail "${name}/seed" "the agent directory was not seeded at $home"
  assertions=$((assertions + 1))
  grep -qF 'claude-sonnet-4.5' "$home/settings.json" ||
    fail "${name}/seed_model" "the seed does not carry the row's model: $(cat "$home/settings.json")"
  assertions=$((assertions + 1))
  grep -qF 'high' "$home/settings.json" ||
    fail "${name}/seed_thinking" "the seed does not carry the row's thinking level"
  assertions=$((assertions + 1))
  # The launch line, so a failed launch is diagnosable from the data root.
  [ -f "$log" ] || fail "${name}/log" "there is no session.log at $log"
  grep -qF "launched $image" "$log" ||
    fail "${name}/launch_line" "the launch line is missing or does not name the image: $(cat "$log")"
  assertions=$((assertions + 1))
  grep -qF 'project trust approved' "$log" ||
    fail "${name}/trust" "the launch line does not record the trust decision: $(cat "$log")"
  assertions=$((assertions + 1))

  # The container really is pi, attached, with the session's agent directory in its environment.
  docker inspect --format '{{.State.Running}}' "s_$id" 2>/dev/null | grep -qx true ||
    fail "${name}/container" "the session's container is not running"
  assertions=$((assertions + 1))
  docker inspect --format '{{json .Config.Cmd}}' "s_$id" 2>/dev/null | grep -qF '"--mode"' ||
    fail "${name}/cmd" "the container is not running pi in RPC mode"
  assertions=$((assertions + 1))
  docker inspect --format '{{json .Config.Env}}' "s_$id" 2>/dev/null |
    grep -qF "PI_CODING_AGENT_DIR=$home" ||
    fail "${name}/env" "the agent directory is not in the container's environment"
  assertions=$((assertions + 1))

  # The exit classifier's third row, from the artifact: removing the container out from under the server ends
  # the attached process, and the row becomes `error` with what pi printed.
  docker rm --force "s_$id" >/dev/null 2>&1 || true
  body=""
  for _ in $(seq 1 300); do
    fetch "${name}_crashed" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_crashed/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_crashed.body")"
    case "$body" in
      *'"status":"error"'*) break ;;
    esac
    sleep 0.1
  done
  printf '%s\n' "$body" | grep -qF '"status":"error"' ||
    fail "${name}_crashed/status" "the removed container did not leave an error row: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"reason":null' &&
    fail "${name}_crashed/reason" "the crash recorded no reason: $body"
  assertions=$((assertions + 1))

  # The delete takes the row, the event log, the worktree and the session's own directory.
  fetch "${name}_delete" DELETE "/api/sessions/$id?force=true" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete/no_answer" "DELETE did not answer"
  expect_status "${name}_delete" "$(answer "${name}_delete")" 202
  # The removal is a background task (the 202 promised it), so the row is polled for its 404 rather than
  # asserted once: a `deleting` row answers 200 and is not a failure.
  local gone=""
  for _ in $(seq 1 300); do
    fetch "${name}_gone" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_gone/no_answer" "GET /api/sessions/$id did not answer"
    gone="$(answer "${name}_gone" | sed -n '1s/^HTTP\/1\.[01] \([0-9][0-9][0-9]\).*/\1/p')"
    [ "$gone" = "404" ] && break
    sleep 0.1
  done
  check "${name}_gone/status" "404" "$gone"
  if docker container inspect "s_$id" >/dev/null 2>&1; then
    fail "${name}/container_left" "the delete left the container behind"
  fi
  assertions=$((assertions + 1))
  if docker network inspect "agents_$id" >/dev/null 2>&1; then
    fail "${name}/network_left" "the delete left the network behind"
  fi
  assertions=$((assertions + 1))
  [ ! -d "$projects/.worktrees/$id" ] ||
    fail "${name}/worktree_left" "the delete left the worktree at $projects/.worktrees/$id"
  assertions=$((assertions + 1))
  [ ! -d "$data/sessions/$id" ] ||
    fail "${name}/session_dir_left" "the delete left the session's own directory at $data/sessions/$id"
  assertions=$((assertions + 1))
  check "$name/registrations" "1" \
    "$(git -C "$repo" worktree list --porcelain | grep -c '^worktree ' || true)"
  docker_session_id=""

  # Its server is left running: `phase_serve` stops whatever is up after the last case.
  rm -rf "$root"
}

# HS-029's proof from the artifact, now over a live agent: a session reaches `idle`, the container dies from
# under it (the row is `error`, the exit classified), and `POST /resume` brings it back to `idle` with the same
# worktree and a new container. The resume's container step is the path that clears the expected-stop flag
# (HS-030) and the launch that replaces an exit nobody is waiting for (HS-031).
#
# Its projects root and data root live under `target/` for `case_agent_launch`'s reason.
case_docker_resume() {
  local name=case_docker_resume body id image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local root="$repo_root/target/verify-resume-$$" projects="$repo_root/target/verify-resume-$$/projects"
  local repo="$repo_root/target/verify-resume-$$/projects/holosmith"
  local data="$repo_root/target/verify-resume-$$/data"
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/agent.rs is the same check)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$repo" || fail "$name/mkdir" "could not create $repo"
  git -C "$repo" init --quiet . || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\nagent_image = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$image" >"$WORK/config-resume-agent.toml"
  start_serve "$WORK/config-resume-agent.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-resume-agent-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  docker_session_id="$id"

  body=""
  for _ in $(seq 1 900); do
    fetch "${name}_row1" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row1/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row1.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "${name}_row1/status" "the first attempt did not reach idle: $body"
  assertions=$((assertions + 1))

  # The container dies from under the server: the attached process ends, and the classifier records an
  # `error` (spec item 6's third row) — which is what makes the resume below a real one.
  docker rm --force "s_$id" >/dev/null 2>&1 || true
  body=""
  for _ in $(seq 1 300); do
    fetch "${name}_row2" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row2/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row2.body")"
    case "$body" in
      *'"status":"error"'*) break ;;
    esac
    sleep 0.1
  done
  printf '%s\n' "$body" | grep -qF '"status":"error"' ||
    fail "${name}_row2/status" "the removed container did not leave an error row: $body"
  assertions=$((assertions + 1))

  fetch_body "${name}_resume" POST "/api/sessions/$id/resume" '' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_resume/no_answer" "POST /api/sessions/$id/resume did not answer"
  expect_status "${name}_resume" "$(answer "${name}_resume")" 202

  # The resume's container step re-creates the network and the container and launches pi again; the row is
  # `idle` only once the handshake answered.
  body=""
  for _ in $(seq 1 900); do
    fetch "${name}_row3" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row3/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row3.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "${name}_row3/status" "the resumed attempt did not reach idle: $body"
  assertions=$((assertions + 1))
  docker inspect --format '{{.State.Running}}' "s_$id" 2>/dev/null | grep -qx true ||
    fail "${name}/container" "the resume left no running container"
  assertions=$((assertions + 1))

  # Cleaning up its own session: the gate's cap is one live session, and this case's would otherwise be the
  # next case's eviction victim.
  fetch "${name}_delete" DELETE "/api/sessions/$id?force=true" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete/no_answer" "DELETE did not answer"
  expect_status "${name}_delete" "$(answer "${name}_delete")" 202
  # The removal is a background task, so it is waited for: removing the case's scratch root underneath it
  # would let the teardown run against paths that are already gone and prove nothing (HS-031's review).
  local gone=""
  for _ in $(seq 1 300); do
    fetch "${name}_gone" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_gone/no_answer" "GET /api/sessions/$id did not answer"
    gone="$(answer "${name}_gone" | sed -n '1s/^HTTP\/1\.[01] \([0-9][0-9][0-9]\).*/\1/p')"
    [ "$gone" = "404" ] && break
    sleep 0.1
  done
  check "${name}_gone/status" "404" "$gone"
  # The session's own directory is gone and **stays** gone: this is the case that deletes a *live* session, so
  # it is where the exit the delete provokes would put `session.log` (and with it the directory) back if the
  # backend did not forget the launch first (HS-031's review, ADR-0039).
  [ ! -d "$data/sessions/$id" ] ||
    fail "${name}/session_dir_left" "the delete left (or re-created) $data/sessions/$id"
  assertions=$((assertions + 1))
  docker_session_id=""
  # Its server is left running: `phase_serve` stops whatever is up after the last case.
  rm -rf "$root"
}

# HS-032's proof from the artifact: a prompt on a Docker session reaches **pi's RPC loop** and what pi answers
# comes back as the row's own reason. The mapping itself (a record in, a frame out) is `cargo test --test rpc`'s,
# against records captured from a real turn; this case is the half only a real container can show.
#
# The assertion is a refusal and not a streamed answer, and that is a fact about this phase rather than a
# shortcut: a session's container is on an `--internal` network (HS-028) and holds no provider key (HS-039), so
# pi's `prompt` command is rejected before a stream exists — "No API key found for openrouter", in pi's own
# words, which is what the row then carries. A refused prompt that reached pi is a *different* observable from
# a session that hangs: the row leaves `running` for `error` with that sentence, and the container is still
# alive afterwards, so the resume below is a second real round trip rather than a repeat of a dead process.
#
# Its projects root and data root live under `target/` for `case_agent_launch`'s reason.
case_docker_turn() {
  local name=case_docker_turn body id image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local root="$repo_root/target/verify-turn-$$" projects="$repo_root/target/verify-turn-$$/projects"
  local repo="$repo_root/target/verify-turn-$$/projects/holosmith"
  local data="$repo_root/target/verify-turn-$$/data"
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/agent.rs is the same check)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$repo" || fail "$name/mkdir" "could not create $repo"
  git -C "$repo" init --quiet . || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\nagent_image = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$image" >"$WORK/config-turn-agent.toml"
  start_serve "$WORK/config-turn-agent.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-turn-agent-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  docker_session_id="$id"

  body=""
  for _ in $(seq 1 900); do
    fetch "${name}_row" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "${name}_row/status" "the session did not become idle: $body"
  assertions=$((assertions + 1))

  # The prompt: accepted by the handler (202, `running`), then ended by pi's own answer.
  fetch_body "${name}_prompt" POST "/api/sessions/$id/prompt" '{"prompt":"say hello"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt/no_answer" "POST /api/sessions/$id/prompt did not answer"
  expect_status "${name}_prompt" "$(answer "${name}_prompt")" 202
  body="$(cat "$WORK/responses/${name}_prompt.body")"
  printf '%s\n' "$body" | grep -qF '"status":"running"' ||
    fail "${name}_prompt/status" "the answer is not a running session: $body"
  assertions=$((assertions + 1))

  # The row leaves `running` — that is the mapping's floor: a turn that never settled would sit here for ever
  # (the deadline is minutes away) — and it leaves with pi's sentence on it.
  body=""
  for _ in $(seq 1 600); do
    fetch "${name}_ended" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_ended/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_ended.body")"
    case "$body" in
      *'"status":"running"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  case "$body" in
    *'"status":"error"'*'"status_reason":"the prompt was refused'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_ended/reason" "the turn did not end with pi's own refusal: $body"
      ;;
  esac
  # The refusal names the provider, which is what proves the sentence came from pi and not from this build.
  printf '%s\n' "$body" | grep -qF 'API key' ||
    fail "${name}_ended/sentence" "the reason does not carry pi's words: $body"
  assertions=$((assertions + 1))

  # pi is still there: the refusal was a command's answer, not the process dying.
  docker inspect --format '{{.State.Running}}' "s_$id" 2>/dev/null | grep -qx true ||
    fail "${name}/container" "the refused prompt left no running container"
  assertions=$((assertions + 1))

  # A second round trip on the same session, so the case proves the RPC path is repeatable rather than a
  # one-off: resume, reach `idle` again, prompt again, and the same refusal comes back.
  fetch_body "${name}_resume" POST "/api/sessions/$id/resume" '' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_resume/no_answer" "POST /api/sessions/$id/resume did not answer"
  expect_status "${name}_resume" "$(answer "${name}_resume")" 202
  body=""
  for _ in $(seq 1 900); do
    fetch "${name}_row2" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row2/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row2.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "${name}_row2/status" "the resumed session did not reach idle: $body"
  assertions=$((assertions + 1))
  fetch_body "${name}_prompt2" POST "/api/sessions/$id/prompt" '{"prompt":"say hello again"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt2/no_answer" "POST /api/sessions/$id/prompt did not answer"
  expect_status "${name}_prompt2" "$(answer "${name}_prompt2")" 202
  body=""
  for _ in $(seq 1 600); do
    fetch "${name}_ended2" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_ended2/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_ended2.body")"
    case "$body" in
      *'"status":"running"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  case "$body" in
    *'"status":"error"'*'the prompt was refused'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_ended2/reason" "the second turn did not end with pi's refusal: $body"
      ;;
  esac

  fetch "${name}_delete" DELETE "/api/sessions/$id?force=true" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete/no_answer" "DELETE did not answer"
  expect_status "${name}_delete" "$(answer "${name}_delete")" 202
  local gone=""
  for _ in $(seq 1 300); do
    fetch "${name}_gone" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_gone/no_answer" "GET /api/sessions/$id did not answer"
    gone="$(answer "${name}_gone" | sed -n '1s/^HTTP\/1\.[01] \([0-9][0-9][0-9]\).*/\1/p')"
    [ "$gone" = "404" ] && break
    sleep 0.1
  done
  check "${name}_gone/status" "404" "$gone"
  docker_session_id=""
  # Its server is left running: `phase_serve` stops whatever is up after the last case.
  rm -rf "$root"
}

# HS-043's real half: a session with a commit on `agent/<id>` and an uncommitted change, deleted from the
# shipped artifact against a real repository. The branch and the snapshot ref survive, the worktree and the
# session directory do not, and the command the 202 carried recovers the work — the story's demo, as a gate.
#
# It needs the daemon and `agent_image` for the session to reach `idle` (a create provisions a real
# container), so a machine without them prints one SKIP line; the keep-rule is still proven there by
# `tests/worktree.rs`, which needs no daemon at all. The cleanup is explicit rather than a trap because a
# gate case that fails should still not leave the next run a container to trip over (HS-041's landmine).
case_delete_keeps_the_work() {
  local name=case_delete_keeps_the_work body id response head show
  local image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local root="$repo_root/target/verify-keeps-$$" projects="$repo_root/target/verify-keeps-$$/projects"
  local repo="$repo_root/target/verify-keeps-$$/projects/holosmith"
  local data="$repo_root/target/verify-keeps-$$/data"
  local worktree branch ref
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/worktree.rs proves the rule without one)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$repo" || fail "$name/mkdir" "could not create $repo"
  git -C "$repo" init --quiet -b main . || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work"
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\nagent_image = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$image" >"$WORK/config-keeps.toml"
  start_serve "$WORK/config-keeps.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-keeps-work-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  docker_session_id="$id"
  worktree="$projects/.worktrees/$id"
  branch="agent/$id"
  ref="refs/holosmith/wip/$id"

  body=""
  for _ in $(seq 1 900); do
    fetch "${name}_row" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "${name}_row/status" "the session did not become idle: $body"
  assertions=$((assertions + 1))

  # The agent's work, simulated — this case is about what a delete keeps, not about what an agent writes:
  # one commit on the branch, and one change the delete's own snapshot is what saves.
  printf 'committed\n' >"$worktree/work.txt" || fail "${name}/commit" "could not write in the worktree"
  git -C "$worktree" -c user.name=verify -c user.email=verify@example add work.txt ||
    fail "${name}/commit" "git add did not work in the worktree"
  git -C "$worktree" -c user.name=verify -c user.email=verify@example commit --quiet \
    -m "the agent's commit" || fail "${name}/commit" "git commit did not work in the worktree"
  printf 'uncommitted\n' >"$worktree/tracked.txt"
  head="$(git -C "$worktree" rev-parse HEAD)"

  fetch "${name}_deleted" DELETE "/api/sessions/$id" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_deleted/no_answer" "DELETE /api/sessions/$id did not answer"
  response="$(answer "${name}_deleted")"
  expect_status "${name}_deleted" "$response" 202
  body="$(cat "$WORK/responses/${name}_deleted.body")"
  case "$body" in
    *"\"kept\":{\"branch\":\"$branch\",\"wip\":\"$ref\",\"recovery\":\"git branch rescue $ref\"}"*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}/kept" "the 202 does not name the branch, the ref and the command: $body"
      ;;
  esac
  wait_for_gone "${name}_gone" "$id"

  check "${name}/branch" "$head" "$(git -C "$repo" rev-parse --verify --quiet "refs/heads/$branch")"
  git -C "$repo" rev-parse --verify --quiet "$ref" >"$WORK/responses/$name.ref"
  [ -s "$WORK/responses/$name.ref" ] ||
    fail "${name}/ref" "the snapshot ref did not survive the delete"
  assertions=$((assertions + 1))
  [ ! -e "$worktree" ] || fail "${name}/worktree" "the worktree is still there: $worktree"
  assertions=$((assertions + 1))
  [ ! -e "$data/sessions/$id" ] || fail "${name}/session_dir" "the session directory is still there"
  assertions=$((assertions + 1))

  # The command the answer printed is the command that works.
  git -C "$repo" branch rescue "$ref" || fail "${name}/rescue" "the recovery command failed"
  show="$(git -C "$repo" show "rescue:tracked.txt")"
  check "${name}/rescue-content" "uncommitted" "$show"

  docker_session_id=""
  docker network rm "agents_$id" >/dev/null 2>&1 || true
  docker rm --force "s_$id" >/dev/null 2>&1 || true
  rm -rf "$root"
  # Its server is left running: `phase_serve` stops whatever is up after the last case.
}

case_reconcile() {
  local name=case_reconcile body id other seqs_checked=1
  local root="$repo_root/target/verify-reconcile-$$"
  local data="$root/data"
  # The counter is a column no client is sent (`Session` is the API's body), so it is read the way
  # `case_exit_classification` reads `expected_stop`. Without python3 the row half still runs and the counter
  # half prints one SKIP line: `tests/reconcile.rs` covers it.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to read sessions.last_seq and the committed seqs (tests/reconcile.rs covers the counter)\n' "$name"
    seqs_checked=0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$root" || fail "$name/mkdir" "could not create $root"
  printf 'owner = "%s"\nmax_live_sessions = 3\nbackend = "stub"\nidle_stop_min = 0\nturn_timeout_min = 0\n' \
    "$owner" >"$WORK/config-reconcile.toml"
  start_serve "$WORK/config-reconcile.toml" "$data"

  # The turn's session, provisioned the way the product provisions one.
  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-reconcile-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  wait_for_idle_id "${name}_idle" "$id"

  # A second session, killed while it is still provisioning: item 7's "any row still in `starting` is either
  # resumed or set to `error`, and never left dangling". The stub spends 200 ms a step, so the kill below lands
  # while this row is `starting` — and the assertion is made *before* the kill, so it cannot pass by luck.
  fetch_body "${name}_create2" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-reconcile-2" ||
    fail "${name}_create2/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create2" "$(answer "${name}_create2")" 202
  body="$(cat "$WORK/responses/${name}_create2.body")"
  other="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$other" ] || fail "${name}_create2/id" "the response names no id: $body"
  fetch "${name}_other" GET "/api/sessions/$other" "Host: 127.0.0.1" ||
    fail "${name}_other/no_answer" "GET /api/sessions/$other did not answer"
  case "$(cat "$WORK/responses/${name}_other.body")" in
    *'"status":"starting"'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_other/starting" "the second session was not provisioning: $(cat "$WORK/responses/${name}_other.body")" ;;
  esac

  # The turn. Its 202 moved the row to `running` before the stub's first frame (200 ms later), so the kill
  # below is in the middle of a turn and not between two of them.
  fetch_body "${name}_prompt" POST "/api/sessions/$id/prompt" '{"prompt":"read the app shell"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt/no_answer" "POST /api/sessions/$id/prompt did not answer"
  expect_status "${name}_prompt" "$(answer "${name}_prompt")" 202
  fetch "${name}_running" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
    fail "${name}_running/no_answer" "GET /api/sessions/$id did not answer"
  case "$(cat "$WORK/responses/${name}_running.body")" in
    *'"status":"running"'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_running" "the prompt did not put the row in running: $(cat "$WORK/responses/${name}_running.body")" ;;
  esac

  # **The crash**: no drain, no signal handler, no last event. `kill -9` is the story's own word.
  kill -9 "$serve_pid" || fail "${name}/kill" "could not kill the server (pid $serve_pid)"
  wait "$serve_pid" 2>/dev/null || true
  serve_pid=""
  # What the kill left, read out of the dead process's own database — the same read the restart's assertions
  # rest on. A turn that had already settled would make `_back/reason` fail for the machine's speed rather
  # than the product's, so the race is named here instead (the window is the stub's 800 ms turn against one
  # HTTP round trip, and a timeout on this would be worth knowing about).
  if [ "$seqs_checked" -eq 1 ]; then
    if ! python3 - "$data/app.db" "$id" "$other" >"$WORK/responses/$name.crash" 2>&1 <<'PYCRASH'
import sqlite3, sys
path, turn, provisioning = sys.argv[1:]
rows = dict(sqlite3.connect(path).execute("SELECT id, status FROM sessions").fetchall())
if rows.get(turn) != "running":
    print(f"the turn had settled before the kill landed: {turn} was {rows.get(turn)}, so this run did not")
    print("test a crash mid-turn (the machine was too slow); tests/reconcile.rs plants the same row")
    sys.exit(1)
if rows.get(provisioning) != "starting":
    print(f"the provision had finished before the kill landed: {provisioning} was {rows.get(provisioning)}")
    sys.exit(1)
print("ok")
PYCRASH
    then
      fail "$name/crash" "$(cat "$WORK/responses/$name.crash")"
    fi
    assertions=$((assertions + 1))
  fi

  # The restart: the shipped artifact again, on the same data root.
  start_serve "$WORK/config-reconcile.toml" "$data"

  # The turn is recorded as a reason, not a state: the row comes back `idle` and *says* the turn was cut.
  body=""
  for _ in $(seq 1 300); do
    fetch "${name}_back" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_back/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_back.body")"
    case "$body" in
      *'"status":"idle"'*) break ;;
    esac
    sleep 0.1
  done
  case "$body" in
    *'"status":"idle"'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_back/status" "the crashed turn's session did not come back idle: $body" ;;
  esac
  case "$body" in
    *'"status_reason":"interrupted"'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_back/reason" "the restarted row does not carry the turn's word: $body" ;;
  esac
  case "$body" in
    *'"status":"interrupted"'*)
      fail "${name}_back/state" "interrupted became a state: $body"
      ;;
    *) assertions=$((assertions + 1)) ;;
  esac

  # The row that was `starting` was resumed, and it is idle rather than dangling.
  wait_for_idle_id "${name}_other_idle" "$other"

  # The transition is in the committed log, not only on the row: a reason on a session.status event.
  fetch_sse_until "${name}_log" "/api/sessions/$id/events" '"reason":"interrupted"' "Host: 127.0.0.1" ||
    fail "${name}_log/no_answer" "GET /api/sessions/$id/events did not answer"
  case "$(cat "$WORK/responses/${name}_log.body")" in
    *'"from":"running","to":"starting","step":null,"reason":"interrupted"'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_log/payload" "the log does not hold the crash transition: $(cat "$WORK/responses/${name}_log.body")" ;;
  esac

  # No row is left claiming a turn is running.
  fetch "${name}_list" GET /api/sessions "Host: 127.0.0.1" ||
    fail "${name}_list/no_answer" "GET /api/sessions did not answer"
  case "$(cat "$WORK/responses/${name}_list.body")" in
    *'"status":"running"'*) fail "${name}_list/running" "a row is still running after the restart" ;;
    *) assertions=$((assertions + 1)) ;;
  esac

  # **V9's seq half**: no duplicate and no missing seq, and the counter is the log's highest.
  if [ "$seqs_checked" -eq 1 ]; then
    if ! python3 - "$data/app.db" "$id" "$other" >"$WORK/responses/$name.seq" 2>&1 <<'PY'
import sqlite3, sys
path, *ids = sys.argv[1:]
db = sqlite3.connect(path)
for session in ids:
    row = db.execute("SELECT last_seq FROM sessions WHERE id = ?", (session,)).fetchone()
    if row is None:
        print(f"{session}: no row")
        sys.exit(1)
    last = row[0]
    seqs = [seq for (seq,) in db.execute(
        "SELECT seq FROM events WHERE session_id = ? ORDER BY seq", (session,)
    )]
    if not seqs:
        print(f"{session}: no committed events")
        sys.exit(1)
    if seqs != list(range(1, len(seqs) + 1)):
        print(f"{session}: the seqs are not 1..n: {seqs}")
        sys.exit(1)
    if last != seqs[-1]:
        print(f"{session}: last_seq {last} is not the highest committed seq {seqs[-1]}")
        sys.exit(1)
print("ok")
PY
    then
      fail "$name/seq" "$(cat "$WORK/responses/$name.seq")"
    fi
    assertions=$((assertions + 1))
  fi

  # Its server is left running: `phase_serve` stops whatever is up after the last case, and the next case that
  # needs its own boot stops it.
  rm -rf "$root"
}

# The health payload: exactly five keys, and the uptime is not predictable, so the assertion is
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

# HS-033: one tool call's full output is a file, and the route serves it — while a name that cannot be a
# filename is refused rather than joined to a path. The file is written here because the stub's turn calls no
# tool: `tests/rpc.rs` is where a real turn's tool output is stored by the pump, and `tests/tool_output.rs`
# where the route's answers are asserted beside it. What only the shipped artifact can add is that the route
# is in *this* binary's table, off *this* data root, with the guards as they ship.
case_tool_output() {
  local name=case_tool_output response tool dir size
  tool=call_00_9hlt8we6459oa420spsp3mju
  dir="$WORK/data/sessions/$created_id/tool"
  mkdir -p "$dir" || fail "$name/mkdir" "could not create $dir"
  # Two megabytes, so the served answer is far larger than any payload and the read stays bounded.
  dd if=/dev/zero bs=1024 count=2048 2>/dev/null | tr '\0' 'x' >"$dir/$tool.txt" ||
    fail "$name/write" "could not write $dir/$tool.txt"
  # The canary one level up, where a join without the id rule lands.
  printf 'not yours' >"$WORK/data/sessions/leak.txt" ||
    fail "$name/canary" "could not write the traversal canary"

  fetch "$name" GET "/api/sessions/$created_id/tool-output/$tool" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET the tool-output route did not answer"
  # The header block alone, read out of the saved answer: `expect_header` pipes its argument through `sed
  # '/^\r$/q'`, which quits at the blank line, so a two-megabyte body in a shell variable would leave the
  # writer with SIGPIPE — and `set -o pipefail` turns that into the whole run exiting 141 (measured here).
  response="$(sed '/^\r$/q' "$WORK/responses/$name.http")"
  expect_status "$name" "$response" 200
  expect_header "$name" "$response" content-type "text/plain; charset=utf-8"
  size="$(wc -c <"$WORK/responses/$name.body")"
  check "$name/bytes" 2097152 "$(printf '%s' "$size" | tr -d ' ')"

  # A name that cannot be a filename is not a path to join: each of these is the envelope's `not_found`, and
  # none of them reaches a file outside the session's tool directory. The stored name is `<id>.txt`, so the id
  # must leave the extension off: `..%2F..%2Fleak` from `sessions/<id>/tool/` is `sessions/leak.txt`, which is
  # where the canary below is — a canary further away could never be reached and every check would pass with the
  # id rule deleted (this story's review's point, and axum really does decode a `%2F` inside a segment).
  name=case_tool_output_traversal
  local id
  for id in '..' '..%2F..%2Fleak' '%2e%2e%2f%2e%2e%2fleak' 'a%2Fb' '%2e%2e' 'with%20space'; do
    fetch "$name" GET "/api/sessions/$created_id/tool-output/$id" "Host: 127.0.0.1" ||
      fail "$name/no_answer" "GET a traversal ($id) did not answer"
    response="$(answer "$name")"
    expect_status "$name" "$response" 404
    expect_header "$name" "$response" content-type "application/json"
    expect_code "$name" "$response" not_found
    case "$(cat "$WORK/responses/$name.body")" in
      *"not yours"*)
        fail "$name/leak" "a traversal reached a file outside the session's tool directory"
        ;;
      *)
        assertions=$((assertions + 1))
        ;;
    esac
  done

  # An output nobody stored, on a session that exists: the same code, its own sentence.
  name=case_tool_output_missing
  fetch "$name" GET "/api/sessions/$created_id/tool-output/call_nothing_here" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET a stored-nothing output did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_code "$name" "$response" not_found
}

# HS-035: an image in a tool result is a file beside the output, and the route serves its bytes with the
# type the file has — not the text route's `text/plain` and not an envelope. The file is planted here for
# the reason `case_tool_output`'s is: the stub's turn calls no tool, so `tests/rpc.rs` is where a real turn's
# image is stored by the pump, and `tests/tool_output.rs` where the route's answers are asserted beside it.
# What only the shipped artifact can add is that the route is in *this* binary's table, off *this* data root,
# that the answer is the picture rather than a description of it, and that a name which cannot be a filename
# is refused instead of joined.
case_tool_image() {
  local name=case_tool_image response tool index dir image
  tool=call_00_9hlt8we6459oa420spsp3mju
  index=1
  dir="$WORK/data/sessions/$created_id/tool"
  image="$dir/$tool-$index.png"
  mkdir -p "$dir" || fail "$name/mkdir" "could not create $dir"
  # A PNG's eight-byte signature and a few bytes after it: enough that a base64 body served verbatim, or the
  # wrong content type, cannot pass.
  printf '\211PNG\r\n\032\n\000\000\000\015IHDRpixels' >"$image" ||
    fail "$name/write" "could not write $image"
  # The canary exactly where a join without the id rule lands: the stored name is `<id>-<index>.png`, so the id
  # `..%2Fleak` becomes `<data>/sessions/<id>/tool/../leak-1.png`.
  printf 'not yours' >"$WORK/data/sessions/$created_id/leak-1.png" ||
    fail "$name/canary" "could not write the traversal canary"

  fetch "$name" GET "/api/sessions/$created_id/tool-image/$tool/$index" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET the tool-image route did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  expect_header "$name" "$response" content-type "image/png"
  cmp -s "$WORK/responses/$name.body" "$image" ||
    fail "$name/bytes" "the served answer is not the file's own bytes"
  assertions=$((assertions + 1))

  # An index nobody stored, on a session that exists: the same code as a missing row, and its own sentence.
  name=case_tool_image_missing
  fetch "$name" GET "/api/sessions/$created_id/tool-image/$tool/7" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET an unstored image did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_code "$name" "$response" not_found
  # An index that is not a number is not a name this store wrote, so it is the same answer rather than a
  # second code an operator has to learn.
  name=case_tool_image_bad_index
  fetch "$name" GET "/api/sessions/$created_id/tool-image/$tool/one" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET a non-numeric index did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_code "$name" "$response" not_found

  name=case_tool_image_traversal
  local id
  for id in '..' '..%2Fleak' '%2e%2e%2fleak' 'a%2Fb' '%2e%2e' 'with%20space'; do
    fetch "$name" GET "/api/sessions/$created_id/tool-image/$id/$index" "Host: 127.0.0.1" ||
      fail "$name/no_answer" "GET a traversal ($id) did not answer"
    response="$(answer "$name")"
    expect_status "$name" "$response" 404
    expect_header "$name" "$response" content-type "application/json"
    expect_code "$name" "$response" not_found
    case "$(cat "$WORK/responses/$name.body")" in
      *"not yours"*)
        fail "$name/leak" "a traversal reached a file outside the session's tool directory"
        ;;
      *)
        assertions=$((assertions + 1))
        ;;
    esac
  done
}

# HS-044's other half, on a real daemon: the names a crash leaves behind that no row owns. The live session's
# container is replaced under the same name (which is what "no name conflict" means), a container and a network
# with no row go, and a worktree directory no row owns goes **while its branch and its snapshot ref stay**
# (HS-043, ADR-0052). The machine's own `hs_egress`/`hs_key` are not the build's session names and are left.
#
# What the SKIP line costs, said out loud: on a machine with no daemon (a hub, which is supported) the container
# and network halves of acceptance bullets 1 and 3 cannot be measured at all — there is nothing to measure — so
# the case prints its one SKIP and passes, exactly as `case_container_mounts`, `case_docker_resume` and
# `case_internet_toggle` do. The *rule* is not skipped with it: `tests/reconcile.rs`'s name tests prove which
# names are the build's and which rows own them, and the worktree half of the sweep is proven there against a
# real repository with no daemon.
#
# It boots its own server because it kills one: `kill -9` takes the attached `docker run` with it, which is the
# leftover spec item 6 names. Its scratch lives under `target/` because the daemon must be able to see the
# projects root a mount comes from (ADR-0035's measurement).
case_reconcile_orphans() {
  local name=case_reconcile_orphans body id image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local fake=9999999999999999
  local root="$repo_root/target/verify-reconcile-orphans-$$"
  local projects="$root/projects" repo="$root/projects/holosmith" data="$root/data"
  local before after branch wip
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/reconcile.rs and the reconcile itself are the same check)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$repo" || fail "$name/mkdir" "could not create $repo"
  git -C "$repo" init --quiet . || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "docker"\nprojects_root = "%s"\nagent_image = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$image" >"$WORK/config-reconcile-orphans.toml"
  start_serve "$WORK/config-reconcile-orphans.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-reconcile-orphans-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  docker_session_id="$id"
  body=""
  for _ in $(seq 1 900); do
    fetch "${name}_idle" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_idle/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_idle.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "${name}_idle/status" "the session did not reach idle: $body"
  assertions=$((assertions + 1))
  before="$(docker inspect --format '{{.Id}}' "s_$id" 2>/dev/null)" ||
    fail "$name/container" "the running session has no container"

  # The orphans, all of them under names no row has. `docker_second_session_id` is what the EXIT trap removes,
  # so a `fail` below cannot leave them behind.
  docker_second_session_id="$fake"
  docker create --name "s_$fake" "$image" true >/dev/null ||
    fail "$name/orphan_container" "could not create the orphan container"
  docker network create --internal "agents_$fake" >/dev/null ||
    fail "$name/orphan_network" "could not create the orphan network"
  mkdir -p "$projects/.worktrees" || fail "$name/mkdir" "could not create the worktrees root"
  git -C "$repo" worktree add --quiet -b "agent/$fake" "$projects/.worktrees/$fake" HEAD ||
    fail "$name/orphan_worktree" "could not make the orphan worktree"
  git -C "$projects/.worktrees/$fake" -c user.name=verify -c user.email=verify@example commit \
    --quiet --allow-empty -m "the agent's work" ||
    fail "$name/orphan_commit" "could not commit in the orphan worktree"
  branch="$(git -C "$repo" rev-parse "agent/$fake")"
  wip="refs/holosmith/wip/$fake"
  git -C "$repo" update-ref "$wip" "$branch" ||
    fail "$name/orphan_ref" "could not write the snapshot ref"

  # The crash, then the restart.
  kill -9 "$serve_pid" || fail "$name/kill" "could not kill the server (pid $serve_pid)"
  wait "$serve_pid" 2>/dev/null || true
  serve_pid=""
  start_serve "$WORK/config-reconcile-orphans.toml" "$data"

  # The sweep runs after the banner and before the first request is answered, so it is waited for: the
  # container and the network both have to go.
  for _ in $(seq 1 240); do
    if ! docker inspect "s_$fake" >/dev/null 2>&1 &&
      ! docker network ls --format '{{.Name}}' | grep -qx "agents_$fake"; then
      break
    fi
    sleep 0.25
  done
  if docker inspect "s_$fake" >/dev/null 2>&1; then
    fail "$name/orphan_container/gone" "the reconcile left a container no row owns"
  fi
  assertions=$((assertions + 1))
  if docker network ls --format '{{.Name}}' | grep -qx "agents_$fake"; then
    fail "$name/orphan_network/gone" "the reconcile left a network no row owns"
  fi
  assertions=$((assertions + 1))
  [ ! -e "$projects/.worktrees/$fake" ] ||
    fail "$name/orphan_worktree/gone" "the reconcile left a worktree directory no row owns"
  assertions=$((assertions + 1))
  # **What a crash must never cost**: the branch and the snapshot ref (HS-043's rule).
  check "$name/branch" "$branch" "$(git -C "$repo" rev-parse --verify --quiet "agent/$fake")"
  check "$name/ref" "$branch" "$(git -C "$repo" rev-parse --verify --quiet "$wip")"

  # The session that was live came back: same name, a **new** container, and the daemon accepted the name —
  # which is what "no name conflict" means.
  body=""
  for _ in $(seq 1 900); do
    fetch "${name}_back" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_back/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_back.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "${name}_back/status" "the restarted session did not reach idle: $body"
  assertions=$((assertions + 1))
  after="$(docker inspect --format '{{.Id}}' "s_$id" 2>/dev/null)" ||
    fail "$name/back/container" "the restarted session has no container"
  [ "$after" != "$before" ] ||
    fail "$name/back/container/replaced" "the restart kept the container the crash left"
  assertions=$((assertions + 1))
  docker inspect --format '{{.State.Running}}' "s_$id" 2>/dev/null | grep -qx true ||
    fail "$name/back/container/running" "the replaced container is not running"
  assertions=$((assertions + 1))
  # The machine's own proxies are not session names, so the sweep must not have taken them.
  docker inspect "hs_egress" >/dev/null 2>&1 ||
    fail "$name/proxy" "the sweep removed the machine's egress proxy"
  assertions=$((assertions + 1))

  # Cleaning up: the session's row, container and network, and the fake pair (already gone).
  fetch "${name}_delete" DELETE "/api/sessions/$id?force=true" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete/no_answer" "DELETE did not answer"
  expect_status "${name}_delete" "$(answer "${name}_delete")" 202
  wait_for_gone "${name}_gone" "$id"
  docker_session_id=""
  docker_second_session_id=""
  rm -rf "$root"
  # Its server is left running: `phase_serve` stops whatever is up after the last case.
}


# HS-036: this machine's isolation, measured. The probe is `tests/selftest.rs`'s subject too, with a bridge
# control; what only the *shipped artifact* can add is that a real boot runs it, that the answer is in the log,
# that the machine's own `egress` network is created (not internal — the proxies attach to it) and that the
# temporary probe network does not outlive the boot. `HOLOSMITH_LOG=debug` for this boot alone: the pass is a
# `debug` line, because a startup that says nothing is the shape `tests/serve_cli.rs` pins at eight lines.
case_network_selftest() {
  local name=case_network_selftest image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local root="$repo_root/target/verify-selftest-$$"
  # `docker::EGRESS_NETWORK`, spelled a second time here because a shell cannot read a Rust constant: the name
  # is the spec's (`egress`), and the case fails loudly if the binary ever picks another.
  local egress=egress
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/selftest.rs is the same check)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$root/projects" || fail "$name/mkdir" "could not create $root"
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\nagent_image = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$root/projects" "$image" >"$WORK/config-selftest.toml"
  HOLOSMITH_LOG=debug
  export HOLOSMITH_LOG
  start_serve "$WORK/config-selftest.toml" "$root/data"
  unset HOLOSMITH_LOG

  grep -q 'network self-test passed' "$serve_err" ||
    fail "$name/passed" "the boot did not report a passing self-test: $(cat "$serve_err")"
  assertions=$((assertions + 1))
  if grep -q 'self-test could not run' "$serve_err"; then
    fail "$name/ran" "the self-test did not run: $(cat "$serve_err")"
  fi
  assertions=$((assertions + 1))

  # HS-038's pair, in the boot's own words: the probe sees the proxy once it is joined — which is what makes the off
  # case's silence evidence — or the machine had no proxy to join, which the boot also says. A run that says
  # neither is a pair that was measured by nobody, and a `debug` line is why this case boots with `HOLOSMITH_LOG=debug`.
  grep -q 'the probe sees the egress proxy once it is joined' "$serve_err" ||
    grep -q 'the egress proxy is not running on this machine' "$serve_err" ||
    fail "$name/pair" "the boot neither saw the joined proxy nor said it had none: $(grep -i 'egress' "$serve_err" | tr '\n' ' ')"
  assertions=$((assertions + 1))

  # HS-039's inverse pair: the key proxy must answer on the probe's network. A boot that did not start it says
  # so, and that is a `warn` rather than a verdict — but a boot that started it and did not see the alias is a
  # key-proxy join bug, which is what this line makes visible.
  grep -q 'the key proxy answers on the probe' "$serve_err" ||
    grep -q 'the key proxy is not running' "$serve_err" ||
    fail "$name/key-pair" "the boot neither saw the key proxy's alias nor said it had none: $(grep -i 'key proxy' "$serve_err" | tr '\n' ' ')"
  assertions=$((assertions + 1))

  # The egress network is the machine's, is not internal, and exists after the boot.
  docker network inspect "$egress" >/dev/null 2>&1 ||
    fail "$name/egress" "the boot created no egress network"
  local internal
  internal="$(docker network inspect "$egress" --format '{{.Internal}}' 2>/dev/null | tr -d '\r')"
  check "$name/egress-not-internal" "false" "$internal"

  # The temporary probe network does not outlive the boot, whatever the outcome.
  if docker network inspect holosmith-selftest >/dev/null 2>&1; then
    fail "$name/left-behind" "the probe network outlived the boot"
  fi
  assertions=$((assertions + 1))
}

# HS-042: the WIP snapshot as the shipped artifact runs it. No daemon, no server: a real repository and a
# real linked worktree, one uncommitted change, and then the four questions the story's acceptance asks —
# the ref points at the change, the branch and HEAD did not move, the index is untouched, and a tree equal
# to HEAD removes the ref. `tests/snapshot.rs` covers the same function; this is the gate's use of it.
case_snapshot() {
  local name=case_snapshot root="$repo_root/target/verify-snapshot-$$" id=0123456789abcdef
  local project="$root/project" worktree="$root/worktree" out head
  rm -rf "$root"
  mkdir -p "$project" || fail "$name/mkdir" "could not create $project"
  git -C "$project" init --quiet -b main . || fail "$name/git" "git init did not work"
  git -C "$project" config user.name verify
  git -C "$project" config user.email verify@example
  printf 'base\n' >"$project/tracked.txt"
  git -C "$project" add tracked.txt
  git -C "$project" commit --quiet -m base || fail "$name/git" "git commit did not work"
  git -C "$project" worktree add --quiet -b "agent/$id" "$worktree" HEAD ||
    fail "$name/worktree" "git worktree add did not work"
  head="$(git -C "$worktree" rev-parse HEAD)"
  printf 'changed but not committed\n' >"$worktree/tracked.txt"
  printf 'new file\n' >"$worktree/new.txt"

  out="$(cd "$worktree" && HS_SESSION_ID="$id" "$artifact" container snapshot)" ||
    fail "$name/take" "container snapshot failed: $out"
  check "$name/take-output" "written refs/holosmith/wip/$id" "$out"
  check "$name/ref-tracked" "changed but not committed" \
    "$(git -C "$project" show "refs/holosmith/wip/$id:tracked.txt")"
  check "$name/ref-new" "new file" \
    "$(git -C "$project" show "refs/holosmith/wip/$id:new.txt")"
  check "$name/branch" "$head" "$(git -C "$worktree" rev-parse HEAD)"
  [ -z "$(git -C "$worktree" diff --cached --name-only)" ] ||
    fail "$name/index" "the snapshot moved the worktree's index"
  assertions=$((assertions + 1))
  git -C "$worktree" status --porcelain | grep -q 'tracked.txt' ||
    fail "$name/worktree" "the snapshot changed the working tree"
  assertions=$((assertions + 1))

  # A tree equal to HEAD is "nothing to keep": the ref goes instead of another commit.
  printf 'base\n' >"$worktree/tracked.txt"
  rm "$worktree/new.txt"
  out="$(cd "$worktree" && HS_SESSION_ID="$id" "$artifact" container snapshot)" ||
    fail "$name/remove" "container snapshot failed on a clean tree: $out"
  check "$name/remove-output" "removed refs/holosmith/wip/$id" "$out"
  if git -C "$project" rev-parse --verify --quiet "refs/holosmith/wip/$id" >/dev/null; then
    fail "$name/ref-left" "the ref survived a clean tree"
  fi
  assertions=$((assertions + 1))
  rm -rf "$root"
}

# HS-039: the key store and the key proxy's refusal path, with no daemon and no internet. The forwarding half
# to a provider is HS-040's (a gate that needs a real key is not a gate); what this case can prove is the part
# that must never be wrong: the file is 0600, `key list` never prints a value, the CLI reads stdin, and a
# request with no dummy (or an unknown provider) is answered without a provider being contacted.
case_key_proxy() {
  local name=case_key_proxy root="$WORK/key-proxy-$$" config="$WORK/config-key-proxy.toml"
  local key='sk-ant-verify-secret' port statuses keyproxy_pid="" mode
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to speak HTTP to the key proxy (tests/key_proxy.rs covers the wire)\n' "$name"
    return 0
  fi
  rm -rf "$root"
  mkdir -p "$root/data" "$root/projects" || fail "$name/mkdir" "could not create $root"
  printf 'data_root = "%s/data"\nprojects_root = "%s/projects"\nbackend = "stub"\n' "$root" "$root" >"$config"

  printf '%s\n' "$key" | "$artifact" --config "$config" key set anthropic >"$WORK/responses/$name-set.out" 2>"$WORK/responses/$name-set.err" ||
    fail "$name/set" "key set failed: $(cat "$WORK/responses/$name-set.err")"
  grep -qx 'stored anthropic' "$WORK/responses/$name-set.out" ||
    fail "$name/set-output" "key set printed $(cat "$WORK/responses/$name-set.out")"
  mode="$(stat -c '%a' "$root/data/secrets/keys.toml" 2>/dev/null || true)"
  check "$name/mode" "600" "$mode"

  "$artifact" --config "$config" key list >"$WORK/responses/$name-list.out" 2>"$WORK/responses/$name-list.err" ||
    fail "$name/list" "key list failed: $(cat "$WORK/responses/$name-list.err")"
  grep -qx 'anthropic' "$WORK/responses/$name-list.out" ||
    fail "$name/list-provider" "key list did not name the provider: $(cat "$WORK/responses/$name-list.out")"
  if grep -q "$key" "$WORK/responses/$name-list.out" "$WORK/responses/$name-list.err"; then
    fail "$name/list-secret" "key list printed the key"
  fi
  assertions=$((assertions + 2))

  port="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
  "$artifact" proxy key --listen "127.0.0.1:$port" --keys "$root/data/secrets/keys.toml" \
    >"$WORK/responses/$name-proxy.log" 2>&1 &
  keyproxy_pid=$!
  for _ in $(seq 1 100); do
    grep -q 'the key proxy is listening' "$WORK/responses/$name-proxy.log" 2>/dev/null && break
    kill -0 "$keyproxy_pid" 2>/dev/null || break
    sleep 0.05
  done
  grep -q "listen=127.0.0.1:$port" "$WORK/responses/$name-proxy.log" ||
    fail "$name/listen" "the key proxy did not report its bound address: $(cat "$WORK/responses/$name-proxy.log")"

  statuses="$(python3 - "$port" <<'PY'
import socket, sys

port = int(sys.argv[1])

def ask(head):
    with socket.create_connection(("127.0.0.1", port), timeout=10) as sock:
        sock.sendall(head.encode())
        answer = b""
        while True:
            chunk = sock.recv(4096)
            if not chunk:
                break
            answer += chunk
    return answer.decode("utf-8", "replace")

questions = [
    ("no-key", "POST /anthropic/v1/messages HTTP/1.1\r\nHost: key-proxy:8080\r\nContent-Length: 2\r\n\r\n{}"),
    ("unknown", "POST /not-a-provider/v1/messages HTTP/1.1\r\nHost: key-proxy:8080\r\n\r\n"),
]
for label, head in questions:
    answer = ask(head)
    print(label, answer.split("\r\n", 1)[0])
PY
)" || fail "$name/driver" "the python3 driver failed"
  check "$name/no-key" "no-key HTTP/1.1 401 Unauthorized" "$(grep '^no-key ' <<<"$statuses")"
  check "$name/unknown" "unknown HTTP/1.1 403 Forbidden" "$(grep '^unknown ' <<<"$statuses")"
  grep -q 'status=no-key' "$WORK/responses/$name-proxy.log" ||
    fail "$name/log" "the key proxy did not log the refusal: $(cat "$WORK/responses/$name-proxy.log")"
  assertions=$((assertions + 1))

  kill -TERM "$keyproxy_pid" 2>/dev/null || true
  wait "$keyproxy_pid" 2>/dev/null || true

  "$artifact" --config "$config" key remove anthropic >/dev/null 2>&1 ||
    fail "$name/remove" "key remove failed"
  [ -z "$("$artifact" --config "$config" key list)" ] ||
    fail "$name/removed" "key list is not empty after remove"
  assertions=$((assertions + 1))
  rm -rf "$root"
}

# HS-040 (V3's bytes): the request the key proxy hands its TLS client — head **and** body. The TLS client is a
# process (`openssl`), so a shim on `PATH` stands in for it: it takes one bounded read of exactly what the proxy
# writes, saves it, and answers a canned 200. What the case can then assert is the thing that was wrong: the
# request carries the body, names its length once, and has the stored key swapped in. No internet, no real key
# and no daemon — a gate cannot have those, and the real forward is HS-040's recorded demo.
case_key_proxy_request() {
  local name=case_key_proxy_request root="$WORK/key-proxy-request-$$"
  local fake="$root/openssl" capture="$root/request.bin" port keyproxy_pid answered
  local body='{"model":"claude-haiku-5-5","max_tokens":8}'
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to speak HTTP to the key proxy (tests/key_proxy.rs covers the wire)\n' "$name"
    return 0
  fi
  rm -rf "$root"
  mkdir -p "$root/data/secrets" "$fake" || fail "$name/mkdir" "could not create $root"
  printf '[providers]\nopencode-go = "sk-opencode-go-verify"\n\n[sessions]\nverify-session = "hs-dummy-verify"\n' \
    >"$root/data/secrets/keys.toml"
  chmod 600 "$root/data/secrets/keys.toml"
  cat >"$fake/openssl" <<'SH'
#!/bin/sh
# Stand in for "openssl s_client": capture what the proxy writes, then answer one complete HTTP response. One
# bounded read rather than a read-until-EOF, because the proxy deliberately keeps our stdin open; the request is
# far below PIPE_BUF and the proxy writes it in one `write_all`, so one read returns all of it — if that ever stops
# being true, `body` fails and names this assumption rather than blaming the proxy.
dd bs=8192 count=1 of="$CAPTURE" 2>/dev/null
printf 'HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok'
SH
  chmod +x "$fake/openssl"
  port="$(python3 - <<'PYFREEPORT'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PYFREEPORT
)"
  rm -f "$capture"
  PATH="$fake:$PATH" CAPTURE="$capture" \
    "$artifact" proxy key --listen "127.0.0.1:$port" --keys "$root/data/secrets/keys.toml" \
    >"$WORK/responses/$name-proxy.log" 2>&1 &
  keyproxy_pid=$!
  for _ in $(seq 1 100); do
    grep -q 'the key proxy is listening' "$WORK/responses/$name-proxy.log" 2>/dev/null && break
    kill -0 "$keyproxy_pid" 2>/dev/null || break
    sleep 0.05
  done
  answered="$(python3 - "$port" "$body" <<'PYASK'
import socket, sys

port, body = int(sys.argv[1]), sys.argv[2].encode()
head = (b"POST /opencode-go/v1/messages HTTP/1.1\r\nHost: key-proxy:8080\r\n"
        b"Content-Type: application/json\r\nx-api-key: hs-dummy-verify\r\n"
        + b"Content-Length: %d\r\n\r\n" % len(body))
with socket.create_connection(("127.0.0.1", port), timeout=20) as sock:
    sock.sendall(head + body)
    answer = b""
    while True:
        chunk = sock.recv(4096)
        if not chunk:
            break
        answer += chunk
print(answer.decode("utf-8", "replace").replace("\r\n", " ").strip()[:120])
PYASK
)" || fail "$name/driver" "the python3 driver failed"
  case "$answered" in
    "HTTP/1.1 200 OK"*" ok") assertions=$((assertions + 1)) ;;
    *) fail "$name/answer" "the proxy did not forward the canned answer: $answered" ;;
  esac
  kill -TERM "$keyproxy_pid" 2>/dev/null || true
  wait "$keyproxy_pid" 2>/dev/null || true
  # Exactly one Content-Length, from the body's real length, and the body after the blank line.
  check "$name/content-length" "1" "$(grep -c '^Content-Length:' "$capture" 2>/dev/null || true)"
  check "$name/content-length-value" "Content-Length: ${#body}" \
    "$(grep '^Content-Length:' "$capture" 2>/dev/null | tr -d '\r' || true)"
  grep -q "^x-api-key: sk-opencode-go-verify" "$capture" 2>/dev/null ||
    fail "$name/key" "the forwarded request does not carry the stored key: $(head -c 400 "$capture")"
  if grep -q 'hs-dummy-verify' "$capture" 2>/dev/null; then
    fail "$name/dummy" "the forwarded request still carries the session's dummy key"
  fi
  assertions=$((assertions + 1))
  if [ "$(tail -c ${#body} "$capture" 2>/dev/null)" != "$body" ]; then
    fail "$name/body" "the forwarded request ends without the body: $(tail -c 200 "$capture" | tr -d '\r')"
  fi
  assertions=$((assertions + 1))
  rm -rf "$root"
}

# HS-037: the egress proxy, on the wire and with no daemon and no internet. What it proves is the four
# refusals spec item 2 names — anything that is not CONNECT, a port that is not 443, a host that is not on
# the allow-list, and a name that resolves inside the machine *even when it is on the list* — plus that the
# log line attributes the request to the session in the proxy username. The allow path needs a real
# allow-listed host, so it is a unit test over a local echo server (`src/egress.rs`) and the story's demo;
# a gate case that claimed to prove it would be proving nothing, or would need the internet.
case_egress_proxy() {
  local name=case_egress_proxy log="$WORK/egress.log" statuses port proxy_pid
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to speak HTTP to the proxy (src/egress.rs covers the wire)\n' "$name"
    return 0
  fi
  # A port the kernel gives us, then released: the proxy binds it a moment later, which is the same bet
  # `tests/` make when they ask for port 0.
  port="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
  "$artifact" proxy egress --listen "127.0.0.1:$port" --allow crates.io --allow localhost \
    >"$log" 2>&1 &
  proxy_pid=$!
  for _ in $(seq 1 100); do
    grep -q 'the egress proxy is listening' "$log" 2>/dev/null && break
    kill -0 "$proxy_pid" 2>/dev/null || break
    sleep 0.05
  done
  grep -q "listen=127.0.0.1:$port" "$log" ||
    fail "$name/listen" "the proxy did not report its bound address: $(cat "$log")"
  # `allow=2` is the list it was given, so a proxy that read nothing is visible.
  grep -q 'allow=2' "$log" || fail "$name/allow" "the proxy did not report its allow-list: $(cat "$log")"

  # Four questions, one driver, one line of the form `<label> status` per answer. The proxy closes each
  # connection after answering, which is why a fresh socket is opened per request.
  statuses="$(python3 - "$port" <<'PY'
import base64, socket, sys

port = int(sys.argv[1])

def ask(head):
    with socket.create_connection(("127.0.0.1", port), timeout=10) as sock:
        sock.sendall(head.encode())
        answer = b""
        while True:
            chunk = sock.recv(4096)
            if not chunk:
                break
            answer += chunk
    return answer.decode("utf-8", "replace")

session = base64.b64encode(b"abcdef0123456789:pw").decode()
questions = [
    ("method", "GET http://crates.io/ HTTP/1.1\r\nHost: crates.io\r\n\r\n"),
    ("port", "CONNECT crates.io:80 HTTP/1.1\r\n\r\n"),
    ("listed", "CONNECT example.com:443 HTTP/1.1\r\nProxy-Authorization: Basic %s\r\n\r\n" % session),
    ("inside", "CONNECT localhost:443 HTTP/1.1\r\n\r\n"),
]
for label, head in questions:
    answer = ask(head)
    print(label, answer.split("\r\n", 1)[0])
PY
)" || fail "$name/driver" "the python3 driver failed"

  check "$name/not-connect" "method HTTP/1.1 405 Method Not Allowed" \
    "$(grep '^method ' <<<"$statuses")"
  check "$name/not-443" "port HTTP/1.1 403 Forbidden" "$(grep '^port ' <<<"$statuses")"
  check "$name/not-listed" "listed HTTP/1.1 403 Forbidden" "$(grep '^listed ' <<<"$statuses")"
  # `localhost` **is** on the allow-list and is refused anyway: the address rule wins (item 2's
  # "regardless of the allow-list"), and this is the assertion that would pass if the check were skipped
  # for a listed name.
  check "$name/inside" "inside HTTP/1.1 403 Forbidden" "$(grep '^inside ' <<<"$statuses")"

  # One line per answer, with the session id from the proxy username, the host and both byte counts: the
  # refusal is what the operator (and this case) can grep.
  grep -q 'session=abcdef0123456789 host=example.com status=not-listed' "$log" ||
    fail "$name/attribution" "the log line did not attribute the request to the session: $(cat "$log")"
  grep -q 'host=localhost status=inside' "$log" ||
    fail "$name/inside-log" "the inside refusal is not in the log: $(cat "$log")"
  grep -q 'bytes_up=0 bytes_down=0' "$log" ||
    fail "$name/bytes" "a refusal did not log its bytes: $(cat "$log")"
  assertions=$((assertions + 3))

  kill -TERM "$proxy_pid" 2>/dev/null || true
  wait "$proxy_pid" 2>/dev/null || true
}

# HS-038 (V6): the Internet-access checkbox is **topology**, not configuration. Two real sessions, one per
# setting, both taken through the production path (a create's network step is what `apply_proxy` joins the
# machine's egress proxy from), and three measurements on each session's own network.
#
# The image has no `curl` (measured), so V6's `curl --noproxy '*'` and "a non-listed host through the proxy" are
# asked by node — the same `net.connect` the self-test's probe is built on, for the same reason ADR-0044 gives.
# What the case asserts, per network:
#
#   direct  the internet, asked directly. **Unreachable in both cases**: the checkbox opens a proxy, it does not
#           give the session a route, and a case that only checked the `on` half would pass a build that had
#           stopped being internal at all.
#   proxy   the egress proxy's alias. It must not resolve in the `off` case (`EAI_AGAIN` when nothing is
#           attached — measured) and must answer `403` for a host that is not on the allow-list in the `on` one.
#
# Then the two halves that no container can show: `docker inspect`'s environment (identical modulo the session
# id, which is the acceptance's "the difference is topology, not configuration") and a `kill` + `resume` pair,
# after which the network's endpoint count is 1 for the off session and 2 for the on one — the assertion a
# wrong flag cannot survive, and the only proof that a resume keeps the choice.
case_internet_toggle() {
  local name=case_internet_toggle image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local root="$repo_root/target/verify-toggle-$$"
  # Two statements, not one `local`: `local` is a command, so `projects="$root/projects"` on the same line would
  # expand `$root` before it is set, and `set -u` makes that an error rather than an empty path (case_backup's
  # lesson).
  local projects="$root/projects"
  local data="$root/data"
  local off_id="" on_id="" body label id verdicts env_off env_on got count want running why pair key_mounts
  # The probe's verdict words, spelled the way the `case` patterns below need them: a shell `$CONNECTING` would be
  # an unbound variable, because the node constants live inside the heredoc where the shell cannot see them.
  local not_listed="HTTP/1.1 403 Forbidden"
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/selftest.rs and tests/container.rs are the same check)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$projects/holosmith" || fail "$name/mkdir" "could not create $projects/holosmith"
  git -C "$projects/holosmith" init --quiet . ||
    fail "$name/git" "git init did not work in $projects/holosmith"
  git -C "$projects/holosmith" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work"
  # `allow = ["crates.io"]` on purpose: a 403 is only evidence of an allow-list if the list is not empty.
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "docker"\nprojects_root = "%s"\ndata_root = "%s"\nagent_image = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\n\n[egress]\nallow = ["crates.io"]\n' \
    "$owner" "$projects" "$data" "$image" >"$WORK/config-toggle.toml"
  # A real key is present for this boot, so "the session never sees it" is measured rather than true by
  # absence. The session's own `models.json` carries the dummy; `hs_key` is the only mount of `keys.toml`.
  printf 'sk-ant-toggle-real-do-not-leak\n' |
    "$artifact" --config "$WORK/config-toggle.toml" key set anthropic >/dev/null ||
    fail "$name/key-set" "could not seed the toggle boot's provider key"
  start_serve "$WORK/config-toggle.toml" "$data"

  # `wait_for_idle_docker`, not `wait_for_idle_id`: a Docker create ends with pi's ready handshake.
  # The two creates. The only difference between them is the body's `network`, which is the point.
  for label in off on; do
    if [ "$label" = off ]; then
      body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high","network":false}'
    else
      body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high","network":true}'
    fi
    fetch_body "${name}_${label}_create" POST /api/sessions "$body" \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-toggle-$label" ||
      fail "${name}_${label}_create/no_answer" "POST /api/sessions did not answer"
    expect_status "${name}_${label}_create" "$(answer "${name}_${label}_create")" 202
    body="$(cat "$WORK/responses/${name}_${label}_create.body")"
    id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
    [ -n "$id" ] || fail "${name}_${label}_create/id" "the response names no id: $body"
    # The row carries the choice, and that is what the topology is derived from (acceptance bullet 4).
    printf '%s\n' "$body" | grep -qF "\"network\":$([ "$label" = on ] && echo true || echo false)" ||
      fail "${name}_${label}_create/network" "the row does not carry the choice it was given: $body"
    assertions=$((assertions + 1))
    # Both ids, so a `fail` between the creates and the deletes leaves neither container nor network behind.
    if [ "$label" = off ]; then
      off_id="$id"
      docker_session_id="$id"
    else
      on_id="$id"
      docker_second_session_id="$id"
    fi
    wait_for_idle_docker "${name}_${label}_idle" "$id"
    docker network inspect "agents_$id" >/dev/null 2>&1 ||
      fail "${name}_${label}_network" "the create left no agents_$id network"
    assertions=$((assertions + 1))
  done

  # The probe, one line per question. `node -e` because this image has node and no curl.
  cat >"$WORK/toggle-probe.js" <<'PROBE'
const net = require("net");
// One line per question, and the verdicts are the shell patterns' contract: `connect` resolves `connected` or a
// code (`ENETUNREACH` on an internal network), and `ask` resolves a status line or a code. The words live here
// and nowhere else in the probe — a constant the shell cannot read is a comment that looks like a source of truth.
const direct = () =>
  new Promise((resolve) => {
    const socket = net.connect({ host: "1.1.1.1", port: 443 });
    socket.setTimeout(3000);
    socket.on("connect", () => {
      socket.destroy();
      resolve("connected");
    });
    socket.on("timeout", () => {
      socket.destroy();
      resolve("timed out");
    });
    socket.on("error", (error) => resolve(String(error.code || error.message)));
  });
const key = () =>
  new Promise((resolve) => {
    const socket = net.connect({ host: "key-proxy", port: 8080 });
    socket.setTimeout(3000);
    socket.on("connect", () => {
      socket.destroy();
      resolve("connected");
    });
    socket.on("timeout", () => {
      socket.destroy();
      resolve("timed out");
    });
    socket.on("error", (error) => resolve(String(error.code || error.message)));
  });
const ask = (head) =>
  new Promise((resolve) => {
    const socket = net.connect({ host: "egress-proxy", port: 3128 });
    let answer = "";
    socket.setTimeout(5000);
    socket.on("connect", () => socket.write(head));
    socket.on("data", (chunk) => {
      answer += chunk;
    });
    socket.on("close", () => resolve(answer.split("\r\n")[0] || "closed"));
    socket.on("timeout", () => {
      socket.destroy();
      resolve("timed out");
    });
    // No auth header: the username is attribution, not a check (ADR-0045), and a probe that had to invent one
    // would be testing the wrong thing.
    socket.on("error", (error) => resolve(String(error.code || error.message)));
  });
(async () => {
  console.log(`direct ${await direct()}`);
  console.log(`key ${await key()}`);
  console.log(`proxy ${await ask("CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r\n")}`);
})();
PROBE

  for label in off on; do
    if [ "$label" = off ]; then id="$off_id"; else id="$on_id"; fi
    verdicts="$(docker run --rm --network "agents_$id" "$image" node -e "$(cat "$WORK/toggle-probe.js")" \
      2>"$WORK/responses/${name}_${label}_probe.err")" ||
      fail "${name}_${label}_probe" "the probe container failed: $(cat "$WORK/responses/${name}_${label}_probe.err")"
    # The pass is a *code*, spelled out rather than "anything that is not one of those two": an unrecognised word
    # from a changed probe would otherwise pass this check silently, which is the vacuous-assertion trap the gate
    # has been bitten by twice (HS-020, HS-061).
    case "$verdicts" in
      *"direct connected"* | *"direct refused"*)
        fail "${name}_${label}_direct" "the session reached 1.1.1.1 directly, so its network is not internal: $verdicts"
        ;;
      *"direct ENETUNREACH"* | *"direct EHOSTUNREACH"* | *"direct ENETDOWN"* | *"direct timed out"*)
        assertions=$((assertions + 1))
        ;;
      *)
        fail "${name}_${label}_direct" "the probe printed no direct verdict this case can read: $verdicts"
        ;;
    esac
    # The key proxy is the one address a session may reach in **both** cases (spec item 2): a model call
    # must work with the Internet-access checkbox off, so a missing alias is a key-proxy join bug rather than
    # a checkbox bug. The probe only proves the TCP alias resolves and accepts; the dummy-key check is
    # `case_key_proxy`'s and the provider path is HS-040's.
    case "$verdicts" in
      *"key connected"*)
        assertions=$((assertions + 1))
        ;;
      *)
        fail "${name}_${label}_key" "the key proxy's alias did not answer on the session network: $verdicts"
        ;;
    esac
    if [ "$label" = off ]; then
      case "$verdicts" in
        *"proxy EAI_AGAIN"* | *"proxy ENOTFOUND"*)
          assertions=$((assertions + 1))
          ;;
        *)
          fail "${name}_off_proxy" "a session that asked for no internet resolved or reached the egress proxy: $verdicts"
          ;;
      esac
    else
      case "$verdicts" in
        *"proxy $not_listed"*)
          assertions=$((assertions + 1))
          ;;
        *)
          # The two causes of "the alias does not answer" are different facts and only one of them is this
          # story's: a machine whose proxy container is **not running** (HS-037's `warn`, a supported machine)
          # and a proxy that runs but was never joined to this session's network — which is the checkbox not
          # working. `EAI_AGAIN` cannot tell them apart; one `docker inspect` can. A SKIP therefore has to name
          # evidence the boot actually printed, or it would be a green run over the bug the case exists for.
          running="$(docker inspect hs_egress --format '{{.State.Running}}' 2>/dev/null | tr -d '\r')"
          if [ "$running" != "true" ]; then
            why="$(grep -i 'egress proxy' "$serve_err" | tr '\n' ' ')"
            [ -n "$why" ] ||
              fail "${name}_on_proxy" "the egress proxy is not running and the boot said nothing about it: $verdicts"
            printf 'SKIP %s/on: the egress proxy is not running, so the on half cannot be measured (the boot said: %s)\n' \
              "$name" "$why"
          else
            fail "${name}_on_proxy" \
              "the egress proxy is running and was not on the on session's network, so the checkbox did not change \
               reachability: $verdicts"
          fi
          ;;
      esac
    fi
  done

  # The real key is in `keys.toml` and nowhere a session can reach: not in the session's directory (which is a
  # mount), not in the container's environment, and not in its mounts. `models.json` is the only file the agent
  # reads that could carry a key, and it carries the dummy.
  real_key='sk-ant-toggle-real-do-not-leak'
  for id in "$off_id" "$on_id"; do
    if grep -R -F "$real_key" "$data/sessions/$id" >/dev/null 2>&1; then
      fail "${name}_$( [ "$id" = "$off_id" ] && echo off || echo on )_key_leak" \
        "the real provider key reached $data/sessions/$id"
    fi
    if docker inspect "s_$id" --format '{{json .Config.Env}} {{json .Mounts}}' 2>/dev/null | grep -F "$real_key" >/dev/null; then
      fail "${name}_$( [ "$id" = "$off_id" ] && echo off || echo on )_container_key" \
        "the real provider key reached the session container's environment or mounts"
    fi
    assertions=$((assertions + 1))
  done
  # The key proxy mounts the **directory** the key file lives in, not the file: `Store` writes `keys.toml` by
  # temp-file-and-rename, and a file bind mount pins the inode, so a proxy started before the first create would
  # keep reading the store as it was at mount time and answer `bad-key` for every session (HS-040 measured it).
  # The assertion names the directory, and that the file is not a source of its own.
  key_mounts="$(docker inspect hs_key --format '{{range .Mounts}}{{.Source}} {{end}}' 2>/dev/null)"
  case "$key_mounts" in
    *secrets/keys.toml*)
      fail "$name/key_mount" "hs_key mounts the key file itself, which a rename cannot reach: $key_mounts"
      ;;
    *"/secrets "*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/key_mount" "hs_key does not mount the key file's directory, so the key proxy cannot read it: $key_mounts"
      ;;
  esac
  assertions=$((assertions + 1))

  # The environment: identical in both cases once each session's own id is normalised — the acceptance's "the
  # difference is topology, not configuration", read off the containers the daemon is really running and over
  # **every** variable rather than the one the story names, because the next story that adds an env var should not
  # have to remember this case.
  env_off="$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "s_$off_id" 2>/dev/null |
    sed "s/$off_id/SESSION/g" | sort)"
  env_on="$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "s_$on_id" 2>/dev/null |
    sed "s/$on_id/SESSION/g" | sort)"
  [ -n "$env_off" ] || fail "${name}_off_env" "the off session's container has no environment to read"
  if [ "$env_off" != "$env_on" ]; then
    fail "${name}_env_parity" "the two containers' environments differ, so the difference is configuration rather \
      than topology: off=[$(printf '%s' "$env_off" | tr '\n' ' ')] on=[$(printf '%s' "$env_on" | tr '\n' ' ')]"
  fi
  assertions=$((assertions + 1))
  # And the one variable the acceptance names, spelled out: a second spelling of the URL in shell on purpose, the
  # way `case_container_mounts` spells the mount vector again (V1 is measured independently of the binary).
  for pair in "off:$off_id" "on:$on_id"; do
    label="${pair%%:*}"
    id="${pair##*:}"
    got="$(printf '%s\n' "$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "s_$id" 2>/dev/null)" |
      sed -n 's/^HTTPS_PROXY=//p')"
    check "${name}_${label}_proxy_env" "http://$id@egress-proxy:3128" "$got"
  done

  # A resume keeps the choice: the container is killed (its exit is `unexpected_exit`, which lands the row in
  # `error` — a resumable state), resumed, and the network's endpoint count is then the choice itself: the session
  # alone when nothing may be reached, and the session plus the machine's proxy when something may.
  #
  # The proxy is **detached first**, and that is what makes the count evidence rather than an inheritance: `resume`
  # re-creates the network idempotently, so a network that survived the stop keeps whatever the create attached to
  # it — an `on` session would report 2 even with `apply_proxy` deleted from `resume`. Detached and then 2 is a
  # rejoin; detached and then 1 is a missing one.
  for label in off on; do
    # The key proxy is on every session network, so the stable count is session + key proxy (2) for the
    # off case and session + key proxy + egress proxy (3) for the on one. The egress checkbox is still the
    # only difference, which is what makes the assertion about it.
    if [ "$label" = off ]; then id="$off_id"; want=2; else id="$on_id"; want=3; fi
    docker network disconnect --force "agents_$id" hs_egress >/dev/null 2>&1 || true
    docker kill "s_$id" >/dev/null 2>&1 || fail "${name}_${label}_kill" "could not kill s_$id"
    body=""
    for _ in $(seq 1 200); do
      fetch "${name}_${label}_error" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
        fail "${name}_${label}_error/no_answer" "GET /api/sessions/$id did not answer"
      body="$(cat "$WORK/responses/${name}_${label}_error.body")"
      case "$body" in
        *'"status":"error"'*) break ;;
      esac
      sleep 0.1
    done
    printf '%s\n' "$body" | grep -q '"status":"error"' ||
      fail "${name}_${label}_error" "the killed container's exit did not move the row to error: $body"
    assertions=$((assertions + 1))
    fetch_body "${name}_${label}_resume" POST "/api/sessions/$id/resume" '' \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "${name}_${label}_resume/no_answer" "POST /api/sessions/$id/resume did not answer"
    expect_status "${name}_${label}_resume" "$(answer "${name}_${label}_resume")" 202
    wait_for_idle_docker "${name}_${label}_resumed" "$id"
    count=""
    for _ in $(seq 1 100); do
      count="$(docker network inspect "agents_$id" --format '{{len .Containers}}' 2>/dev/null | tr -d '\r')"
      [ "$count" = "$want" ] && break
      sleep 0.1
    done
    check "${name}_${label}_resumed_endpoints" "$want" "$count"
  done

  # The case leaves the machine as it found it: both sessions gone (their containers, networks and worktrees with
  # them), and only then is the trap's backstop disarmed — disarming it before the deletes would leave a failed
  # delete with nothing to clean it up.
  remove_session "${name}_off_delete" "$off_id" force
  remove_session "${name}_on_delete" "$on_id" force
  docker_session_id=""
  docker_second_session_id=""
  if docker network inspect "agents_$off_id" >/dev/null 2>&1 ||
    docker network inspect "agents_$on_id" >/dev/null 2>&1; then
    fail "${name}_left_behind" "a deleted session left its network behind"
  fi
  assertions=$((assertions + 1))
  # Its server is left running: `phase_serve` stops whatever is up after the last case, and that server's data root
  # is this case's — so the scratch tree goes now, like `case_docker_turn`'s.
  rm -rf "$root"
}

# HS-075: a database from a **newer** release. `serve` continues — item 10's rollback must not need a restore, and
# expand/contract is what makes continuing safe — and it says so in two places an operator reads: the banner's
# `schema <n>` is the *recorded* number rather than the binary's, and one `warn` names both versions and the command
# that refuses (`doctor`, HS-045). This is the observable half of the acceptance's third bullet; the pure verdict
# and its sentence are `tests/schema_previous.rs`'s.
# HS-041 (spec item 3, V7): the preview listener. Two halves.
#
# The first needs nothing but the booted artifact: a **second listener** that serves only `/<host>/<id>/*`,
# refuses every control API path (with a sentence, not the envelope), refuses another machine's host name,
# refuses a session id no row holds, and refuses an upgrade whose `Origin` is not the preview origin.
#
# The second is V7 itself: a real container on an `--internal` network, fetched **from the host** through the
# shipped artifact, plus a WebSocket round trip over the same path. It needs the daemon and the agent image and
# prints one SKIP line without them. The bug this case exists to catch — a preview listener that does not
# proxy — fails the first half, so a SKIP for "no daemon" is not that bug (the rule in docs/build.md).
#
# The container is the case's own `docker run` under the name and network the convention gives a session's
# container (`s_<id>` on `agents_<id>`), because the shared boot's backend is the *stub*: the question here is
# what the preview listener does with that address, not what the backend would have built there.
case_preview() {
  local name=case_preview response body id label image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local upstream_port asset ws

  # This case needs a boot of its own, for two reasons a shared one cannot give it: a **stub** backend, so the
  # create below needs no project on disk (a Docker boot refuses a project the machine does not hold, and which
  # one the boot before this left running is not this case's business), and a data root of its own, so the row it
  # makes is invisible to every other case. The shared boot is put back afterwards, because the cases after this
  # one talk to `$port` and the sessions `$created_id` names.
  printf 'backend = "stub"\npreview_listen = "127.0.0.1:0"\n' >"$WORK/config-preview.toml"
  if [ -n "$serve_pid" ] && kill -0 "$serve_pid" 2>/dev/null; then
    stop_serve
  fi
  start_serve "$WORK/config-preview.toml" "$WORK/data-preview"
  [ -n "${preview_port:-}" ] ||
    fail "$name/banner" "the banner named no preview listener: $(cat "$serve_err")"

  # ---- the first half: what this listener is not -------------------------------------------------
  preview_fetch "$name-api-health" GET /api/health ||
    fail "$name/api_health" "GET /api/health did not answer on the preview port"
  response="$(answer "$name-api-health")"
  expect_status "$name/api_health" "$response" 404
  body="$(cat "$WORK/responses/$name-api-health.body")"
  case "$body" in
    *'"code"'*) fail "$name/api_health" "the preview listener answered the control API's envelope: $body" ;;
    *'/local/'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/api_health" "the refusal names no preview path: $body" ;;
  esac
  label="$name-api-sessions"
  preview_fetch "$label" GET /api/sessions || fail "$label" "GET /api/sessions did not answer"
  expect_status "$name/api_sessions" "$(answer "$label")" 404
  label="$name-app-shell"
  preview_fetch "$label" GET / || fail "$label" "GET / did not answer"
  response="$(answer "$label")"
  expect_status "$name/app_shell" "$response" 404
  if cmp -s "$WORK/responses/$label.body" web/dist/index.html; then
    fail "$name/app_shell" "the preview listener served the control UI's app shell"
  fi
  assertions=$((assertions + 1))

  # Another machine's host name, a segment that is not a session id, and an id no row holds.
  label="$name-other-host"
  preview_fetch "$label" GET "/studio/0123456789abcdef/" || fail "$label" "did not answer"
  expect_status "$name/other_host" "$(answer "$label")" 404
  label="$name-bad-id"
  preview_fetch "$label" GET "/local/notahexid/" || fail "$label" "did not answer"
  expect_status "$name/bad_id" "$(answer "$label")" 404
  label="$name-unknown-session"
  preview_fetch "$label" GET "/local/0123456789abcdef/" || fail "$label" "did not answer"
  expect_status "$name/unknown_session" "$(answer "$label")" 404

  # Item 3's one rule: an upgrade's Origin must be the preview origin.
  label="$name-foreign-origin"
  preview_fetch "$label" GET "/local/0123456789abcdef/socket" \
    "Origin: http://evil.example" "Upgrade: websocket" "Connection: Upgrade" ||
    fail "$label" "did not answer"
  response="$(answer "$label")"
  expect_status "$name/foreign_origin" "$response" 403
  body="$(cat "$WORK/responses/$label.body")"
  case "$body" in
    *"evil.example"*"http://localhost:8081"*) assertions=$((assertions + 1)) ;;
    *)
      case "$body" in
        *"http://localhost:8081"*"evil.example"*) assertions=$((assertions + 1)) ;;
        *) fail "$name/foreign_origin" "the refusal names neither origin: $body" ;;
      esac
      ;;
  esac
  label="$name-no-origin"
  preview_fetch "$label" GET "/local/0123456789abcdef/socket" \
    "Upgrade: websocket" "Connection: Upgrade" || fail "$label" "did not answer"
  expect_status "$name/no_origin" "$(answer "$label")" 403

  # The session screen's link: the shipped page's own script carries the path it builds and the origin key it
  # reads. The link is drawn by the page, so the bytes to assert are the asset's, not the shell's.
  fetch "$name-shell" GET / "Host: 127.0.0.1" || fail "$name/shell" "GET / did not answer"
  asset="$(sed -n 's/.*<script[^>]*src="\([^"]*\.js\)".*/\1/p' "$WORK/responses/$name-shell.body" |
    sed -n '1p')"
  [ -n "$asset" ] || fail "$name/asset" "the app shell names no script: $(cat "$WORK/responses/$name-shell.body")"
  fetch "$name-asset" GET "$asset" "Host: 127.0.0.1" || fail "$name/asset" "GET $asset did not answer"
  expect_status "$name/asset" "$(answer "$name-asset")" 200
  # Three bytes of the link, each of which a deleted feature takes away: the class the anchor is given
  # (`className="preview"` — esbuild minifies identifiers, not string literals), the health key it builds its URL
  # from (`t.preview_origin`), and the host segment itself (the bundle keeps it as `"local"`, and the template
  # that uses it is one character of arithmetic away from unreadable — the *shape* of the path is
  # `web/test/transcript.test.ts`'s, which is where a rule is tested).
  for ws in "preview_origin" '"local"' 'className="preview"'; do
    if ! grep -q -F -- "$ws" "$WORK/responses/$name-asset.body"; then
      fail "$name/link" "the shipped page carries no $ws: the session screen's preview link is not in it"
    fi
    assertions=$((assertions + 1))
  done

  # ---- the second half: V7 itself ----------------------------------------------------------------
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon, so V7 is not measured here (the first half ran)\n' "$name"
    stop_serve
    start_serve
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s, so V7 is not measured here (the first half ran)\n' "$name" "$image"
    stop_serve
    start_serve
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 for the WebSocket round trip (the first half ran)\n' "$name"
    stop_serve
    start_serve
    return 0
  fi
  # From the config this case's own boot read, not the shared one: a stub listening on a port the listener does
  # not forward to would report the disagreement as a preview failure.
  upstream_port="$("$artifact" config show --config "$WORK/config-preview.toml" |
    sed -n 's/^preview_upstream_port = \([0-9][0-9]*\)$/\1/p')"
  [ -n "$upstream_port" ] ||
    fail "$name/upstream_port" "config show names no preview_upstream_port"

  # A session row, through the API: the id the preview path names, and the reason a refusal is not a 404.
  fetch_body "$name-create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-preview-1" ||
    fail "$name/create" "POST /api/sessions did not answer"
  expect_status "$name/create" "$(answer "$name-create")" 202
  id="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/$name-create.body" | sed -n '1p')"
  [ -n "$id" ] || fail "$name/create" "the create names no id: $(cat "$WORK/responses/$name-create.body")"
  # Scored for the trap, in the order a failure can happen: the container and the network are both ours.
  docker_session_id="$id"

  docker network create --internal "agents_$id" >/dev/null 2>&1 ||
    fail "$name/network" "docker network create agents_$id did not work"
  docker run --detach --rm --name "s_$id" --network "agents_$id" \
    --env "HS_PREVIEW_TEST_PORT=$upstream_port" "$image" node -e '
const http = require("http");
const crypto = require("crypto");
const GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const server = http.createServer((request, response) => {
  response.writeHead(200, { "content-type": "text/plain" });
  response.end("hello from a container at " + request.url + "\n");
});
server.on("upgrade", (request, socket) => {
  const key = request.headers["sec-websocket-key"] || "";
  const accept = crypto.createHash("sha1").update(key + GUID).digest("base64");
  socket.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n" +
    "Sec-WebSocket-Accept: " + accept + "\r\n\r\n");
  socket.on("data", (buffer) => {
    const opcode = buffer[0] & 0x0f;
    const masked = (buffer[1] & 0x80) !== 0;
    const length = buffer[1] & 0x7f;
    if (!masked || length === 0) return;
    const mask = buffer.subarray(2, 6);
    const payload = Buffer.from(buffer.subarray(6, 6 + length));
    for (let index = 0; index < payload.length; index += 1) payload[index] ^= mask[index % 4];
    if (opcode === 0x8) { socket.end(); return; }
    socket.write(Buffer.concat([Buffer.from([0x81, payload.length]), payload]));
  });
});
server.listen(Number(process.env.HS_PREVIEW_TEST_PORT), "0.0.0.0");
' >/dev/null 2>&1 || fail "$name/container" "docker run for s_$id did not work"
  # The container's own address on its network is the only way to know it is up: `docker inspect` is where the
  # real flags and addresses are visible (the landmine), and the container appears seconds after `run` returns.
  local address="" waited=0
  while [ "$waited" -lt 100 ]; do
    address="$(docker inspect "s_$id" \
      --format "{{with index .NetworkSettings.Networks \"agents_$id\"}}{{.IPAddress}}{{end}}" 2>/dev/null || true)"
    [ -z "$address" ] || break
    sleep 0.1
    waited=$((waited + 1))
  done
  [ -n "$address" ] || fail "$name/address" "s_$id has no address on agents_$id"
  # The same question the listener asks, answered by the host directly: V7's measurement, once more, before the
  # listener's answer is read — so a failure says which half is wrong.
  if ! timeout 5 python3 -c "
import http.client, sys
connection = http.client.HTTPConnection(sys.argv[1], int(sys.argv[2]), timeout=4)
connection.request('GET', '/')
print(connection.getresponse().status)
" "$address" "$upstream_port" >"$WORK/preview-direct.txt" 2>&1; then
    fail "$name/host_reach" "the host could not reach $address:$upstream_port: $(cat "$WORK/preview-direct.txt")"
  fi
  check "$name/host_reach" 200 "$(cat "$WORK/preview-direct.txt")"

  label="$name-through"
  preview_fetch "$label" GET "/local/$id/" || fail "$label" "did not answer"
  response="$(answer "$label")"
  expect_status "$name/through" "$response" 200
  body="$(cat "$WORK/responses/$label.body")"
  # The body is the container's own bytes, chunk framing and all: a tunnel relays them rather than re-encoding
  # them, so the assertion is that the sentence arrived, not that it is the whole body.
  case "$body" in
    *"hello from a container at /"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/through" "the container's answer did not arrive through the preview listener: $body" ;;
  esac

  # The WebSocket half: a hand-written handshake (the image has no `ws` package and this build has no WebSocket
  # crate), one masked frame each way, and the preview origin as the `Origin`.
  timeout 20 python3 - "$preview_port" "$id" "http://localhost:8081" >"$WORK/preview-websocket.txt" 2>&1 <<'PYWS' ||
import base64
import os
import socket
import sys

port, session, origin = int(sys.argv[1]), sys.argv[2], sys.argv[3]
key = base64.b64encode(os.urandom(16)).decode()
connection = socket.create_connection(("127.0.0.1", port), timeout=10)
connection.sendall(
    (
        f"GET /local/{session}/socket HTTP/1.1\r\n"
        f"Host: localhost:{port}\r\n"
        f"Origin: {origin}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"Sec-WebSocket-Key: {key}\r\n\r\n"
    ).encode()
)
received = b""
while b"\r\n\r\n" not in received:
    received += connection.recv(4096)
head, rest = received.split(b"\r\n\r\n", 1)
status = head.split(b"\r\n")[0].decode()
print(status)
if "101" not in status:
    print(head.decode(errors="replace"))
    raise SystemExit(1)
payload = b"ping-through-the-preview"
mask = os.urandom(4)
masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
connection.sendall(bytes([0x81, 0x80 | len(payload)]) + mask + masked)
while len(rest) < 2:
    rest += connection.recv(4096)
length = rest[1] & 0x7F
while len(rest) < 2 + length:
    rest += connection.recv(4096)
print("echo " + rest[2 : 2 + length].decode())
PYWS
    fail "$name/websocket" "the WebSocket round trip failed: $(cat "$WORK/preview-websocket.txt")"
  case "$(sed -n '1p' "$WORK/preview-websocket.txt")" in
    "HTTP/1.1 101 Switching Protocols"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/websocket" "the handshake was not answered 101: $(cat "$WORK/preview-websocket.txt")" ;;
  esac
  if ! grep -qx 'echo ping-through-the-preview' "$WORK/preview-websocket.txt"; then
    fail "$name/websocket" "no frame came back through the preview listener: $(cat "$WORK/preview-websocket.txt")"
  fi
  assertions=$((assertions + 1))

  # What this case made of the machine, it takes back: the container and its network. No `DELETE` of the session
  # is needed — this boot's data root dies with the work dir — but the container outlives the process, and a
  # network with a live endpoint refuses to be removed, so the container goes first.
  docker rm --force "s_$id" >/dev/null 2>&1 || true
  docker network rm "agents_$id" >/dev/null 2>&1 || true
  docker_session_id=""
  stop_serve
  start_serve
}

case_schema_newer() {
  local name=case_schema_newer root="$repo_root/target/verify-schema-newer-$$"
  local line status
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to write a newer schema_version (tests/schema_previous.rs covers the verdict)\n' "$name"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$root"
  # `data_root` is in the *file* as well as in `start_serve`'s `--data-dir`, because the `doctor` run below reads
  # the config and nothing else: a `--data-dir` is a `serve` override, and the doctor must be pointed at the same
  # database rather than at the machine's `/var/lib/holosmith`.
  # A `hub` on purpose: its `doctor` list has no Docker or git check (item 10 §6), so the doctor run below is the
  # same on a machine with a daemon and on one without — the database is the only thing this case breaks.
  printf 'owner = "%s"\nrole = "hub"\nbackend = "stub"\ndata_root = "%s"\ncontrol_listen = "127.0.0.1:0"\npreview_listen = "127.0.0.1:0"\nmin_free_gb = 1\n' \
    "$owner" "$root/data" >"$WORK/config-schema-newer.toml"
  # One boot to create and migrate the database, then the row is rewritten to a version from the future.
  start_serve "$WORK/config-schema-newer.toml" "$root/data" >/dev/null
  stop_serve
  python3 - "$root/data/app.db" <<'SQL' || fail "$name/seed" "could not write a newer schema_version"
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("INSERT INTO schema_version (version, name, applied_at) VALUES (99, 'from-the-future', 0)")
db.commit()
SQL
  # No `HOLOSMITH_LOG` here: the default filter is already `info`, which is what a `warn` needs, and the banner is
  # written with `eprintln!` and cannot be filtered at all.
  start_serve "$WORK/config-schema-newer.toml" "$root/data" >/dev/null
  grep -q '^database .* schema 99$' "$serve_err" ||
    fail "$name/banner" "the banner does not print the recorded schema: $(grep '^database ' "$serve_err" || true)"
  assertions=$((assertions + 1))
  # One line, naming what an operator has to act on: both numbers, and the command that refuses.
  line="$(grep -i 'newer release wrote this database' "$serve_err" || true)"
  [ -n "$line" ] || fail "$name/warn" "the boot said nothing about the newer schema: $(cat "$serve_err")"
  case "$line" in
    *99*4*) assertions=$((assertions + 1)) ;;
    *) fail "$name/versions" "the warning does not name both schema versions: $line" ;;
  esac
  printf '%s\n' "$line" | grep -q 'doctor' ||
    fail "$name/doctor" "the warning does not name the command that refuses: $line"
  assertions=$((assertions + 1))
  # …and the command keeps the promise the warning made (HS-045): `doctor --quick` refuses the same database,
  # read **without** migrating it, with `schema::verdict`'s one sentence. `serve` continues (the 200 below),
  # `doctor` does not — that split is ADR-0047's whole decision.
  status=0
  "$artifact" doctor --quick --config "$WORK/config-schema-newer.toml" \
    >"$WORK/$name.doctor.out" 2>&1 || status=$?
  check "$name/doctor/exit" 1 "$status"
  grep -q '^database: fail: ' "$WORK/$name.doctor.out" ||
    fail "$name/doctor/database" "doctor did not refuse the newer database: $(cat "$WORK/$name.doctor.out")"
  assertions=$((assertions + 1))
  grep -q 'newer release wrote' "$WORK/$name.doctor.out" ||
    fail "$name/doctor/sentence" "the refusal is not the warning's sentence: $(cat "$WORK/$name.doctor.out")"
  assertions=$((assertions + 1))
  # One `database` line, and it is the failure. Counting *every* failure here would let a hub whose Tailscale is
  # logged out fail this case under the schema's name (a legitimate `fail` that is not this case's subject) — the
  # rule that a case asserts its own guard's answer, not another's.
  line="$(grep -c '^database: ' "$WORK/$name.doctor.out" || true)"
  check "$name/doctor/one_database_line" 1 "$line"
  # And it is really serving: a refused boot would have exited, so a 200 here is the "continues" half of the claim.
  fetch "$name" GET /api/health "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/health did not answer over a newer database"
  expect_status "$name" "$(answer "$name")" 200
  # Its server is left running for the phase's final `stop_serve` — that is the pattern every case that booted its
  # own uses — and the scratch tree goes now, holding the data root it was migrated from.
  rm -rf "$root"
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
  # Seven deltas: three thinking, then two text, then two more after the tool call. Seven because the run's
  # stub paces one frame per 200 ms and the writer commits every 25 ms, so no two deltas share a commit and
  # coalescing (HS-059) has nothing to merge; a slower window would make this a shorter list.
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

# HS-059's proof from the artifact: a turn's log holds one delta row per run of deltas the writer
# committed together — never more rows than the script emitted — and every word the script said is still
# there, in order, under one message id.
#
# What this case cannot show is the merge itself. The stub paces one frame per 200 ms, eight commit windows
# apart, so nothing here is coalesced: the merge is `tests/turns.rs`'s (seven scripted deltas -> three rows
# at a pace below the window) and `tests/events.rs`'s (5,000 -> three). What the shipped artifact can prove
# is the contract a merge must keep, which is what a burst producer (HS-032's container output) is held to.
#
# It creates its own session and deletes it, and it runs **before** `case_create_session`: the run's cap is one
# live session, so a create here after the shared one exists would evict it and every later case that prompts
# `created_id` would answer `409 session_stopped` (measured: exactly that, the first time this case was
# placed after `case_prompt_turn`).
case_event_coalescing() {
  local name=case_event_coalescing response body id deltas texts
  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-coalescing-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  wait_for_idle_id "${name}_idle" "$id"

  fetch_body "${name}_prompt" POST "/api/sessions/$id/prompt" '{"prompt":"read the app shell"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt/no_answer" "POST /api/sessions/$id/prompt did not answer"
  expect_status "${name}_prompt" "$(answer "${name}_prompt")" 202
  fetch_sse_until "${name}_stream" "/api/sessions/$id/events" '"from":"running","to":"idle"' \
    "Host: 127.0.0.1" || fail "${name}_stream/no_answer" "the stream did not answer"
  body="$(cat "$WORK/responses/${name}_stream.body")"

  # The replay is the log, so an empty one would make every assertion below vacuous.
  deltas="$(printf '%s\n' "$body" | tr -d '\r' | grep -c '^event: *message\.delta' || true)"
  [ "${deltas:-0}" -gt 0 ] ||
    fail "${name}/deltas" "the replay holds no delta, so nothing below can say anything"
  assertions=$((assertions + 1))
  # A merge can only ever remove rows: more delta rows than the script's seven deltas would be a coalescer
  # that invented events.
  [ "$deltas" -le 7 ] ||
    fail "${name}/rows" "the turn wrote $deltas delta rows for seven scripted deltas"
  assertions=$((assertions + 1))
  # One message id per turn, on every delta's payload — the shape a merge must keep (ADR-0018).
  texts="$(printf '%s\n' "$body" | tr -d '\r' | grep '^data: ' | sed -n 's/^data: .*"message_id":"\([^"]*\)".*/\1/p' | sort -u | wc -l | tr -d ' ')"
  [ "$texts" = "1" ] ||
    fail "${name}/message_id" "the turn's deltas name $texts message ids, not one"
  assertions=$((assertions + 1))
  # Nothing is lost and nothing is reordered: the first thinking sentence and the last text sentence are in
  # the replay, in that order, once the text is read back the way the transcript reads it.
  texts="$(printf '%s\n' "$body" | tr -d '\r' | grep '^data: ' |
    sed -n 's/^data: .*"text":"\(.*\)"}$/\1/p' | tr -d '\n')"
  case "$texts" in
    *"The prompt names the app shell."*"Nothing else in it moved."*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}/text" "the replay's text is not the script's, in order: $texts"
      ;;
  esac

  # Leave nothing behind: this session is the case's, not a candidate for a later case's eviction.
  fetch "${name}_delete" DELETE "/api/sessions/$id?force=true" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete/no_answer" "DELETE /api/sessions/$id did not answer"
  expect_status "${name}_delete" "$(answer "${name}_delete")" 202
  wait_for_gone "${name}_gone" "$id"
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

# wait_for_idle_docker <label> <id>: the same question with a Docker create's patience.
#
# Ninety seconds and not `wait_for_idle_id`'s ten: a Docker create runs four steps and ends with pi's ready
# handshake, which is what `case_docker_turn` also allows 90 s for. A slow *first* create on a machine that has to
# unpack the image is not a failure of the story that asked. Top-level rather than nested inside its one caller,
# so the name exists on a machine where that case SKIPs at its guard.
wait_for_idle_docker() {
  local label="$1" id="$2" body="" _
  for _ in $(seq 1 900); do
    fetch "$label" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "$label/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/$label.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF '"status":"idle"' ||
    fail "$label/status" "the session did not reach idle: $body"
  assertions=$((assertions + 1))
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

# remove_session <label> <id> [force]: delete a session and wait for its row to be gone, so a case
# that made one of its own leaves the machine as it found it.
#
# `force` is HS-019's `?force=true`, and a Docker run needs it: the WIP snapshot the delete asks the backend
# for before it removes anything is HS-042's, so a Docker session with a worktree answers `409 unsaved_work`
# until then — the spelling `case_docker_turn` uses, and the reason the stub is where the snapshot's own
# refusal is exercised.
remove_session() {
  local label="$1" id="$2" force="${3:-}" response path
  path="/api/sessions/$id"
  [ -z "$force" ] || path="$path?force=true"
  fetch "$label" DELETE "$path" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$label/no_answer" "DELETE $path did not answer"
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

# HS-043: the delete's own answer, and the page that shows it. Two facts are worth asserting on every
# machine and neither needs a repository: the 202 carries `kept` — three explicit nulls when nothing
# survived, because a stub session has no repository at all (ADR-0015) and a client must be able to tell
# "nothing was kept" from `kept: null`, which is "the repository could not answer" — and the page the
# artifact embeds carries the affordance, the forced-delete button and the sentence that puts the answer's
# own command on screen. The keep-rule itself, against a real repository, is `case_delete_keeps_the_work`
# (daemon and image) and `tests/worktree.rs` (no daemon at all).
#
# It makes its own session, because `case_delete_session` removed the shared one, and it removes it again:
# `case_idle_stop` (next) assumes an empty table and the cap is one.
case_delete_keeps() {
  local name=case_delete_keeps body id response needle
  fetch_body "${name}_created" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-keeps-1" ||
    fail "${name}_created/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_created")"
  expect_status "${name}_created" "$response" 202
  body="$(cat "$WORK/responses/${name}_created.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_created/id" "the response names no id: $body"
  wait_for_idle_id "${name}_created" "$id"

  fetch "${name}_deleted" DELETE "/api/sessions/$id" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_deleted/no_answer" "DELETE /api/sessions/$id did not answer"
  response="$(answer "${name}_deleted")"
  expect_status "${name}_deleted" "$response" 202
  expect_header "${name}_deleted" "$response" content-type "application/json"
  body="$(cat "$WORK/responses/${name}_deleted.body")"
  case "$body" in
    *'"kept":{"branch":null,"wip":null,"recovery":null}'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}/kept" "the 202 does not carry the keep-rule's answer: $body"
      ;;
  esac
  wait_for_gone "${name}_gone" "$id"

  # The page in bytes. `case_app_shell` proves the served page is `web/dist`; these are the story's own
  # strings: the affordance, the second request a refused snapshot earns, the sentence that renders the
  # answer's command, and the notice for a delete that kept nothing. A page without them is the gap this
  # story closes, whatever the API answers.
  for needle in "Delete session" "Delete anyway" "recover it with" "nothing was kept"; do
    grep -qF -- "$needle" web/dist/assets/*.js ||
      fail "${name}/page" "the shipped page does not carry \"$needle\" in web/dist/assets/*.js"
    assertions=$((assertions + 1))
  done
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

  # Settle the session that took the victim's slot: the cap is one, so whatever runs next finds a live
  # `idle` session it may evict rather than a `starting` one it may not (`case_exit_classification` takes
  # that slot and gives it back, which is what leaves `case_create_at_cap_refused` free to make its own
  # `starting` holder).
  wait_for_idle_id "${name}_fresh" "$fresh"
}

# HS-030's proof from the shipped artifact: the stop the server performs records that it was its own, and
# a new container starts without the last one's flag.
#
# The artifact cannot show the story's other half — a container's *exit* — because this build has no process
# whose exit anybody observes: the container runs detached on `sleep infinity` until HS-031 attaches pi's
# stdio, and `--rm` takes it away a moment after it stops (ADR-0038). What it can show is the flag that
# makes the classification possible, and the two things that set and clear it, read out of `app.db` because
# no client is sent it.
#
# It makes its own victim and its own replacement and removes both, and it runs *before*
# `case_create_at_cap_refused` on purpose: that case holds the slot with a session the slow stub leaves
# `starting`, which is live and not evictable, so a create after it answers `409 cap_reached`. Here the
# slot holds `case_create_at_cap_evicts`' settled session, which a create may evict — and this case leaves
# the cap free for the refusal's own holder.
case_exit_classification() {
  local name=case_exit_classification response body victim fresh flag file found
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'
  local database="$WORK/data/app.db"

  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to read the flag back (tests/exit.rs covers this)\n' "$name"
    return 0
  fi

  # Schema 4's column. `ALTER TABLE ... ADD COLUMN` appends the column to the table's own text in
  # `sqlite_master`, which is how `case_events_table` reads `last_seq`; while serve is running that text may
  # still be in app.db-wal, so the glob covers the sidecars.
  found=""
  for file in "$WORK"/data/app.db*; do
    [ -f "$file" ] || continue
    if grep -aq 'expected_stop' "$file"; then
      found=yes
      break
    fi
  done
  check "$name/column" "yes" "$found"

  fetch_body "${name}_victim" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-exit-1" ||
    fail "${name}_victim/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_victim")"
  expect_status "${name}_victim" "$response" 202
  body="$(cat "$WORK/responses/${name}_victim.body")"
  victim="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$victim" ] || fail "${name}_victim/id" "the response names no id: $body"
  wait_for_idle_id "${name}_victim_idle" "$victim"

  # The stop a request can make happen: with the cap at one and the victim idle, this create evicts it, and
  # an eviction goes through the same stop routine the sweeper and the ladder's forced rung use.
  fetch_body "${name}_fresh" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-exit-2" ||
    fail "${name}_fresh/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_fresh")"
  expect_status "${name}_fresh" "$response" 202
  body="$(cat "$WORK/responses/${name}_fresh.body")"
  fresh="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$fresh" ] || fail "${name}_fresh/id" "the response names no id: $body"
  [ "$fresh" != "$victim" ] ||
    fail "${name}_fresh/id" "the create answered the session it had to evict: $body"
  wait_for_stopped_id "${name}_victim_stopped" "$victim"
  # The row moves before the container does, and the stop — and so the flag's write — is a task of its own
  # that runs after it (ADR-0024's ordering, ADR-0038's flag). So the flag is *polled*, not read once: a
  # status that says `stopped` is not a promise that the spawned stop has run yet.
  flag=""
  for _ in $(seq 1 50); do
    flag="$(expected_stop_of "$database" "$victim")"
    [ "$flag" = 1 ] && break
    sleep 0.1
  done
  check "$name/evicted_flag" "1" "$flag"

  # And a new container does not inherit it: a resume frees no slot, so the session that took the victim's
  # is removed first, then the victim is resumed through the real endpoint and its provisioning's container
  # step clears the flag.
  remove_session "${name}_removed_fresh" "$fresh"
  fetch_body "${name}_resume" POST "/api/sessions/$victim/resume" '' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_resume/no_answer" "POST /api/sessions/$victim/resume did not answer"
  expect_status "${name}_resume" "$(answer "${name}_resume")" 202
  wait_for_idle_id "${name}_resumed_idle" "$victim"
  flag="$(expected_stop_of "$database" "$victim")"
  check "$name/resumed_flag" "0" "$flag"

  remove_session "${name}_removed_victim" "$victim"
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

# usage_of <data dir> <session id>: print the row's two token counts as `<in> <out>`, or `none` when the
# row is gone.
#
# The tally is a row fact no client sees — `Session` is pinned to its fifteen keys and phase 1b may not change
# the API (ADR-0038) — so `app.db` is the only place to read it, with the same `python3` reader
# `expected_stop_of` and `case_compaction` use, and the same reason a case SKIPs without one.
usage_of() {
  python3 - "$1/app.db" "$2" <<'PY'
import sqlite3, sys
database, session = sys.argv[1], sys.argv[2]
connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
row = connection.execute(
    "select usage_in, usage_out from sessions where id = ?", (session,)
).fetchone()
print("none" if row is None else f"{row[0]} {row[1]}")
PY
}

# HS-034's proof from the shipped artifact: a session's spend is a column that survives the two things this
# machine really does to a session — an eviction that stops it, and a resume that brings it back. The numbers
# come from the stub's script (1200 in, 340 out per turn), and they are read out of `app.db` because the API
# does not carry them: `Session` is pinned to its fifteen keys (ADR-0038), so the row is the honest witness.
#
# Its own boot, for the reason `case_token_budget` has one: the shared config has a cap of one live session and
# a case that owns both of its rows can evict one without guessing what came before it.
case_usage_survives_resume() {
  local name=case_usage_survives_resume body first second counts
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'

  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to read app.db\n' "$name"
    return 0
  fi
  # Leave the shared boot, as `case_token_budget` does, and leave this one running for whatever follows.
  stop_serve
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\n' "$owner" >"$WORK/config-usage.toml"
  start_serve "$WORK/config-usage.toml" "$WORK/data-usage"

  fetch_body "${name}_first" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-usage-first" ||
    fail "${name}_first/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_first" "$(answer "${name}_first")" 202
  body="$(cat "$WORK/responses/${name}_first.body")"
  first="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$first" ] || fail "${name}_first/id" "the response names no id: $body"
  wait_for_idle_id "${name}_first_idle" "$first"

  fetch_body "${name}_prompt" POST "/api/sessions/$first/prompt" '{"prompt":"read the app shell"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt/no_answer" "POST /api/sessions/$first/prompt did not answer"
  expect_status "${name}_prompt" "$(answer "${name}_prompt")" 202
  wait_for_idle_id "${name}_spent" "$first"

  # The row the turn's `message.end` was added to. Polled, because the writer commits on its own tick: a read
  # taken the instant the row says `idle` can be one frame short (HS-023's own lesson).
  counts=none
  for _ in $(seq 1 100); do
    counts="$(usage_of "$WORK/data-usage" "$first")"
    [ "$counts" = "none" ] || [ "$counts" = "0 0" ] || break
    sleep 0.1
  done
  check "${name}_spent" "1200 340" "$counts"

  # Evicted by the cap: a second create takes the only slot, and the first session is *stopped* rather than
  # deleted, which is what leaves a row to resume.
  fetch_body "${name}_second" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-usage-second" ||
    fail "${name}_second/no_answer" "the second POST /api/sessions did not answer"
  expect_status "${name}_second" "$(answer "${name}_second")" 202
  body="$(cat "$WORK/responses/${name}_second.body")"
  second="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$second" ] || fail "${name}_second/id" "the response names no id: $body"
  # `wait_for_reason_id` is the *turn* policies' shape (back to `idle` with a reason); an eviction leaves the
  # row `stopped`, which is what makes it resumable.
  wait_for_stopped_id "${name}_evicted" "$first"
  case "$(cat "$WORK/responses/${name}_evicted.body")" in
    *'"status_reason":"evicted"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_evicted/reason" "the evicted session's row names no reason: $(cat "$WORK/responses/${name}_evicted.body")"
      ;;
  esac

  # The cap is checked before the transition (ADR-0022: a resume refuses rather than evicting), so the second
  # session has to go before the first can come back. `case_delete_session` is the delete's own case; here the
  # delete is the slot's, and `remove_session` is the house way to take one.
  remove_session "${name}_delete" "$second"

  fetch_body "${name}_resume" POST "/api/sessions/$first/resume" "" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_resume/no_answer" "POST /api/sessions/$first/resume did not answer"
  expect_status "${name}_resume" "$(answer "${name}_resume")" 202
  wait_for_idle_id "${name}_resumed" "$first"

  counts="$(usage_of "$WORK/data-usage" "$first")"
  check "${name}_resumed" "1200 340" "$counts"
}

# expected_stop_of <database> <session>: print one row's expected-stop flag, or `none` when the row is
# gone.
#
# The flag is a row fact no client sees (ADR-0038: `Session` is pinned to its fifteen keys and phase 1b may
# not change the API), so the database is the only place it can be read from — the same `python3` reader
# `age_session_events` uses, and the same reason the case SKIPs without one.
expected_stop_of() {
  python3 - "$1" "$2" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
row = connection.execute(
    "SELECT expected_stop FROM sessions WHERE id = ?", (sys.argv[2],)).fetchone()
connection.close()
print("none" if row is None else row[0])
PY
}

# age_session_events <database> <session> <seconds>: push one session's whole log into the past.
#
# The one thing the gate cannot do through the API. An event's `created_at` is the **producer's**, and the
# shipped stub writes `now`, so "completed more than 24 hours ago" (spec item 15) cannot be produced by a
# request. `python3` with stdlib `sqlite3` is the tool this machine has, and it needs no dependency — the
# case SKIPs without it rather than asserting less (docs/build.md's optional-tool rule).
age_session_events() {
  python3 - "$1" "$2" "$3" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
connection.execute(
    "UPDATE events SET created_at = created_at - ? WHERE session_id = ?",
    (int(sys.argv[3]), sys.argv[2]))
connection.commit()
connection.close()
PY
}

# wait_for_snapshot <label> <database> <session>: poll the log until a `message.snapshot` row exists, then
# print "<snapshots> <deltas>".
#
# A poll and not a wait: the pass is immediate at startup but it runs beside the listener, so "the banner
# printed" does not mean "the fold committed". The counts are printed rather than asserted here so the case
# can `check` them, which is what keeps a failure's message about the rows.
wait_for_snapshot() {
  local label="$1" database="$2" session="$3" counts
  for _ in $(seq 1 100); do
    counts="$(python3 - "$database" "$session" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
rows = dict(connection.execute(
    "SELECT type, count(*) FROM events WHERE session_id = ? GROUP BY type",
    (sys.argv[2],)).fetchall())
connection.close()
print(rows.get("message.snapshot", 0), rows.get("message.delta", 0))
PY
)"
    case "$counts" in
      0\ *) sleep 0.1 ;;
      *) printf '%s\n' "$counts" && return 0 ;;
    esac
  done
  fail "$label/snapshot" "no message.snapshot row appeared in $database"
}

# HS-060's proof from the artifact: a message completed more than a day ago becomes one
# `message.snapshot`, and a client that connects afterwards is replayed the snapshot instead of the deltas.
#
# It boots its own server, like `case_token_budget`, and for a reason the story forces: the window is the
# spec's fixed 24 hours (ADR-0032 — not a config key), so the only way to show it from a shipped artifact is
# to stop the server, age the log it wrote, and start it again. That restart is also the story's own demo.
case_compaction() {
  local name=case_compaction response body id counts
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'
  local database="$WORK/data-compaction/app.db"
  local whole='The shell is the session screen and the list. Reading it now.The composer posts the prompt for real now. Nothing else in it moved.'

  # The guard before `stop_serve`, for the reason `case_worktree` gives: a case that skipped after stopping
  # the run's server would leave `phase_serve`'s own `stop_serve` with no pid, and `kill -TERM ""` fails the
  # gate with a message about SIGTERM.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to age the log and read it back (tests/compaction.rs covers this)\n' "$name"
    return 0
  fi

  stop_serve
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\n' "$owner" \
    >"$WORK/config-compaction.toml"
  start_serve "$WORK/config-compaction.toml" "$WORK/data-compaction"

  fetch_body "$name" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-compaction-1" ||
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
  # The settle is the last frame of the turn, so waiting for it is waiting for the whole message to be
  # committed — and it is the needle the replay below uses again.
  fetch_sse_until "${name}_turn" "/api/sessions/$id/events" '"from":"running","to":"idle"' \
    "Host: 127.0.0.1" ||
    fail "${name}_turn/no_answer" "GET /api/sessions/$id/events did not answer"

  stop_serve
  age_session_events "$database" "$id" 90000
  start_serve "$WORK/config-compaction.toml" "$WORK/data-compaction"
  counts="$(wait_for_snapshot "$name" "$database" "$id")"
  # One snapshot, no deltas left: the story's first two acceptance lines, on the rows.
  check "$name/rows" "1 0" "$counts"

  # The wire half: a client connecting after the boundary is replayed one snapshot holding the whole
  # message, and the frames compaction must not touch are still there.
  fetch_sse_until "${name}_replay" "/api/sessions/$id/events" '"from":"running","to":"idle"' \
    "Host: 127.0.0.1" ||
    fail "${name}_replay/no_answer" "GET /api/sessions/$id/events did not answer"
  expect_status "${name}_replay" "$(answer "${name}_replay")" 200
  body="$(cat "$WORK/responses/${name}_replay.body")"
  case "$body" in
    *message.snapshot*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_replay/snapshot" "the replay carries no snapshot frame: $body" ;;
  esac
  case "$body" in
    *"\"text\":\"$whole\""*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_replay/text" "the snapshot does not hold the whole message: $body" ;;
  esac
  case "$body" in
    *message.delta*) fail "${name}_replay/delta" "a delta frame reached a client after compaction: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  case "$body" in
    *message.end*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_replay/end" "compaction took the message's end frame with it: $body" ;;
  esac
}

# HS-061's two halves from the artifact: a client that opens the stream with no `Last-Event-ID` gets the
# newest `history_window` messages and a `history.truncated` frame naming where its replay starts, and
# `GET .../events/history?before=` answers the page below that cursor.
#
# It boots its own server, like `case_compaction`: the window is a config key, and the shared boot must keep
# the default window so that its replay reaches the beginning of its own log. `history_window = 1` makes the
# smallest log that truncates — two turns — which is also the cheapest one to drive.
case_history() {
  local name=case_history response body id cut first
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'

  stop_serve
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\nhistory_window = 1\n' "$owner" \
    >"$WORK/config-history.toml"
  start_serve "$WORK/config-history.toml" "$WORK/data-history"

  fetch_body "${name}_create" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-history-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  wait_for_idle_id "${name}_idle" "$id"

  # Two turns, so the window of one has something to cut. Each prompt's 202 has already moved the row to
  # `running`, so polling it is what waits for that turn rather than for an earlier one's settle frame.
  local turn
  for turn in 1 2; do
    fetch_body "${name}_prompt_$turn" POST "/api/sessions/$id/prompt" '{"prompt":"read the app shell"}' \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "${name}_prompt_$turn/no_answer" "POST /api/sessions/$id/prompt did not answer"
    expect_status "${name}_prompt_$turn" "$(answer "${name}_prompt_$turn")" 202
    wait_for_idle_id "${name}_idle_$turn" "$id"
  done

  # No `Last-Event-ID`: the case the story is about. The needle is the frame's own payload, because the
  # `event:` line alone would end the read before the cursor it names.
  fetch_sse_until "${name}_stream" "/api/sessions/$id/events" '"before_seq"' "Host: 127.0.0.1" ||
    fail "${name}_stream/no_answer" "GET /api/sessions/$id/events did not answer"
  expect_status "${name}_stream" "$(answer "${name}_stream")" 200
  body="$(cat "$WORK/responses/${name}_stream.body")"
  case "$body" in
    *history.truncated*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_stream/truncated" "a replay of two messages under a window of one sent no truncation frame: $body" ;;
  esac
  # Both turns' boundaries sit below the cut, because the cut is the second turn's own message: the frame
  # says two turns came before the replay, which is what keeps its turn labels true.
  case "$body" in
    *'"turns_before":2'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_stream/turns_before" "the truncation frame did not count the turns it skipped: $body" ;;
  esac
  cut="$(printf '%s\n' "$body" | sed -n 's/.*"before_seq":\([0-9][0-9]*\).*/\1/p' | sed -n '1p')"
  [ -n "$cut" ] || fail "${name}_stream/cursor" "the truncation frame named no cursor: $body"

  # The replay begins exactly where the frame said: the first `id:` in the stream is the cut. Read to the
  # second turn's settle, which is unique here because the first turn is below the cut.
  fetch_sse_until "${name}_replay" "/api/sessions/$id/events" '"reason":"agent_settled"' "Host: 127.0.0.1" ||
    fail "${name}_replay/no_answer" "GET /api/sessions/$id/events did not answer"
  body="$(cat "$WORK/responses/${name}_replay.body")"
  first="$(printf '%s\n' "$body" | sed -n 's/^id: *//p' | sed -n '1p')"
  check "${name}_replay/first_seq" "$cut" "$first"

  # The page below that cursor: the first turn, which the replay did not carry.
  fetch "${name}_page" GET "/api/sessions/$id/events/history?before=$cut&limit=200" "Host: 127.0.0.1" ||
    fail "${name}_page/no_answer" "GET /api/sessions/$id/events/history did not answer"
  response="$(answer "${name}_page")"
  expect_status "${name}_page" "$response" 200
  expect_header "${name}_page" "$response" content-type application/json
  body="$(cat "$WORK/responses/${name}_page.body")"
  case "$body" in
    *'"type":"message.delta"'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_page/delta" "the page below the cursor carries no message: $body" ;;
  esac
  case "$body" in
    *'"has_more":false'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_page/has_more" "the whole of a one-turn page claimed there was more: $body" ;;
  esac
  # The page starts at the log's first row, so no turn boundary sits below it.
  case "$body" in
    *'"turns_before":0'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_page/turns_before" "a page beginning at seq 1 counted a turn before it: $body" ;;
  esac
  case "$body" in
    *'"before_seq":1'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}_page/before_seq" "the page did not name its own lowest seq as the next cursor: $body" ;;
  esac

  # A cursor that names no place in the log is refused with the envelope, not silently defaulted: "the
  # newest page" is what the stream already gave the client.
  fetch "${name}_bad_before" GET "/api/sessions/$id/events/history?before=0" "Host: 127.0.0.1" ||
    fail "${name}_bad_before/no_answer" "GET /api/sessions/$id/events/history did not answer"
  response="$(answer "${name}_bad_before")"
  expect_status "${name}_bad_before" "$response" 422
  expect_header "${name}_bad_before" "$response" content-type application/json
  expect_code "${name}_bad_before" "$response" bad_query
}

# HS-065's proof from the artifact: with `project_backup` on, the first session of a day bundles the
# project repository once, a second session on the same day adds nothing, the bundle is one git can
# restore, and the prune keeps the last seven days per project.
#
# It boots its own server, like `case_history`: `project_backup` is a config key, and the shared boot must
# keep the default (off) so its creates cost nothing. The backend is `stub`, so the create needs no daemon
# and no worktree — the bundle is of the host checkout, which is the same path a Docker run would use.
case_backup() {
  local name=case_backup response body backups id
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'
  # Two statements, not one `local`: with `set -u`, `local a=... b="$a"` expands `$a` before `local`
  # runs, so the second name would be unbound.
  local projects="$WORK/projects-backup"
  local repo="$projects/holosmith"
  backups="$WORK/data-backup/backups"

  stop_serve
  mkdir -p "$repo" "$backups"
  git -C "$repo" init --quiet || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet --allow-empty \
    -m "the first commit" || fail "$name/git" "git commit did not work in $repo"

  # Eight days of bundles that are already there, so the prune has something to remove. They are named
  # like bundles rather than being bundles: the retention rule is the *name*, which is what makes it
  # cheap on a machine that is short of space, and a test of it should not pretend otherwise.
  local day
  for day in 1 2 3 4 5 6 7 8; do
    : >"$backups/holosmith-2020-01-0$day.bundle"
  done

  printf 'owner = "%s"\nmax_live_sessions = 3\nbackend = "stub"\nproject_backup = true\nprojects_root = "%s"\nidle_stop_min = 0\n' \
    "$owner" "$projects" >"$WORK/config-backup.toml"
  start_serve "$WORK/config-backup.toml" "$WORK/data-backup"

  # Two sessions on one day. The second must find the first's bundle and add nothing.
  local key
  for key in verify-backup-1 verify-backup-2; do
    fetch_body "${name}_$key" POST /api/sessions "$create_body" \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: $key" ||
      fail "${name}_$key/no_answer" "POST /api/sessions did not answer"
    expect_status "${name}_$key" "$(answer "${name}_$key")" 202
  done

  # Eight stale days plus today's is nine, so the two oldest go and the week that remains ends today.
  check "$name/kept" 7 "$(ls "$backups" | wc -l | tr -d ' ')"
  if [ -f "$backups/holosmith-2020-01-01.bundle" ]; then
    fail "$name/prune_oldest" "the oldest bundle survived the prune"
  fi
  if [ -f "$backups/holosmith-2020-01-02.bundle" ]; then
    fail "$name/prune_second" "the second oldest bundle survived the prune"
  fi
  if [ ! -f "$backups/holosmith-2020-01-03.bundle" ]; then
    fail "$name/prune_kept" "the prune took a bundle inside the seven it keeps"
  fi
  assertions=$((assertions + 3))

  # Exactly one bundle that is not one of the eight planted ones: the day's.
  check "$name/one_for_the_day" 1 \
    "$(ls "$backups" | { grep -v '^holosmith-2020-' || true; } | wc -l | tr -d ' ')"
  id="$(ls "$backups" | { grep -v '^holosmith-2020-' || true; } | sed -n '1p')"
  git -C "$repo" bundle verify "$backups/$id" >/dev/null 2>&1 ||
    fail "$name/verify" "git could not read the bundle the create kept: $id"

  # The bundle is a recovery point, so it carries the branch: `--all` is the story's own flag.
  if ! git -C "$repo" bundle list-heads "$backups/$id" | grep -q 'refs/heads/'; then
    fail "$name/heads" "the bundle carries no branch: $(git -C "$repo" bundle list-heads "$backups/$id")"
  fi
  assertions=$((assertions + 1))

  # A project with no repository at all: the story's third line is that the failure is **logged** and the
  # session is created anyway. Both halves are asserted, because a `warn` nothing greps is a `warn` that
  # can be deleted without a test noticing.
  fetch_body "${name}_missing" POST /api/sessions \
    '{"project":"absent","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-backup-absent" ||
    fail "${name}_missing/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_missing" "$(answer "${name}_missing")" 202
  if ! grep -q "the project's backups are not up to date" "$serve_err"; then
    fail "$name/logged" "a failed bundle was not logged: $(tail -5 "$serve_err")"
  fi
  assertions=$((assertions + 1))
}

# fake_docker_ping <socket>: a unix socket that answers the preflight's `GET /_ping` with `200 OK`, and then
# keeps listening for the next one. Prints the pid it left behind.
#
# The gate's only use of python3, and it exists because of one order of events: the Docker preflight refuses a
# machine with no daemon *before* anything can be provisioned, and the worktree step `case_worktree` is about
# runs on the host and never talks to Docker. A socket that says what dockerd says is the smallest thing that
# gets past the gate without inventing a flag that skips the check (which ADR-0028 forbids). A machine without
# python3 gets one SKIP line from that case and the rest of the gate runs on: `cargo test` covers the behaviour
# either way, and `tests/worktree.rs` is where it is asserted.
fake_docker_ping() {
  python3 - "$1" <<'PY' >/dev/null 2>&1 &
import os, socket, sys

path = sys.argv[1]
try:
    os.unlink(path)
except FileNotFoundError:
    pass
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(path)
server.listen(8)
while True:
    connection, _ = server.accept()
    connection.recv(4096)
    connection.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
    connection.close()
PY
  echo $!
}

# free_port: a TCP port the kernel just gave us on loopback, printed and released — the same bet `--port 0`
# makes. Something could take it in between, which is why a case that uses one *asserts* the port's answer
# (a `pass` naming it free) rather than assuming it.
free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

# HS-025's proof from the artifact: the worktree step is host git, so a create makes `.worktrees/<id>` on
# branch `agent/<id>` from the repository's HEAD, records both on the row, and — because the *next* step is
# the session's network, which from HS-028 runs `docker network create` against the socket this case answers
# with a `_ping` shim and nothing else — the provisioner takes the worktree back, leaving the registration
# clean. (Before HS-028 the network step refused outright, naming its story in the reason; now it fails on a
# daemon that cannot create a network, so the reason names the step instead of a story number.)
#
# It boots its own server, like `case_token_budget`: the backend is `docker` and the projects root has to be a
# real repository, which the shared boot's stub neither needs nor should carry.
case_worktree() {
  local name=case_worktree response body id base
  local projects="$WORK/projects" repo="$WORK/projects/holosmith"
  # The guard comes first, before `stop_serve`: a case that skipped after stopping the run's server would
  # leave `phase_serve`'s own `stop_serve` with no pid to signal, and a blessed configuration (python3 is
  # optional) would fail the gate at the last assertion with a message about SIGTERM.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to answer the docker preflight (tests/worktree.rs covers this)\n' "$name"
    return 0
  fi
  stop_serve
  mkdir -p "$repo"
  git -C "$repo" init --quiet || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet --allow-empty \
    -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  base="$(git -C "$repo" rev-parse HEAD)" || fail "$name/git" "git rev-parse did not work in $repo"

  socket_pid="$(fake_docker_ping "$WORK/docker.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker.sock" ] && break
    sleep 0.05
  done
  # `min_free_gb = 1` because this machine's own free space is not the case's subject, and
  # `idle_stop_min = 0` because this boot lasts a second and a sweeper would only be noise.
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$WORK" >"$WORK/config-worktree.toml"
  start_serve "$WORK/config-worktree.toml" "$WORK/data-worktree"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-worktree-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"

  # The row is the record: the path, the base commit and the reason the next step refused. The step itself is
  # a background task's, so the row is what the wait is on.
  body=""
  for _ in $(seq 1 100); do
    fetch "${name}_row" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  expect_status "${name}_row" "$(answer "${name}_row")" 200
  printf '%s\n' "$body" | grep -qF "\"worktree\":\"$projects/.worktrees/$id\"" ||
    fail "${name}_row/worktree" "the row does not record the worktree path: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_row/base" "the row does not record the branch's base commit: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"status":"error"' ||
    fail "${name}_row/status" "the session did not end error after the network step refused: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF 'the network step' ||
    fail "${name}_row/reason" "the failure is not the network step's: $body"
  assertions=$((assertions + 1))

  # Teardown, which the provisioner runs because the network step failed: the directory, the branch and the
  # registration are all gone. `<projects>` holds only the repository again.
  [ ! -d "$projects/.worktrees/$id" ] ||
    fail "${name}/worktree_left" "the refused session left its worktree at $projects/.worktrees/$id"
  assertions=$((assertions + 1))
  if git -C "$repo" rev-parse --verify --quiet "refs/heads/agent/$id" >/dev/null; then
    fail "${name}/branch_left" "the refused session left branch agent/$id behind"
  fi
  assertions=$((assertions + 1))
  check "${name}/registration" 1 "$(git -C "$repo" worktree list --porcelain | grep -c '^worktree ')"

  # A delete whose worktree is already gone is not a failure: `force` turns the snapshot refusal (HS-033's)
  # into the removal, and the teardown runs over an absent worktree and branch.
  fetch "${name}_delete" DELETE "/api/sessions/$id?force=true" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete/no_answer" "DELETE /api/sessions/$id did not answer"
  expect_status "${name}_delete" "$(answer "${name}_delete")" 202
  wait_for_gone "${name}_gone" "$id"

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# HS-026's proof from the artifact: three unsafe repositories, each refused with its own code before a row
# exists. The three checks are read on the create's own path, so what this case asserts is the 422 itself
# and the absence of everything a create would have left behind.
#
# It boots its own Docker run over its own projects root, because the run's server was started over a
# projects root with no repository at all and a case must own the state it asserts.
case_worktree_refusal() {
  local name=case_worktree_refusal response body pair project code
  local projects="$WORK/projects-refusal"
  # The guard comes first, before `stop_serve`, for the reason `case_worktree` gives: a skipped case that
  # stopped the server leaves the phase's own teardown with no pid to signal.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to answer the docker preflight (tests/worktree.rs covers this)\n' "$name"
    return 0
  fi
  stop_serve

  # 1. A `.git` that is a file: a linked worktree's or a submodule checkout's shape.
  local linked="$projects/linked"
  mkdir -p "$linked"
  git -C "$linked" init --quiet || fail "$name/git" "git init did not work in $linked"
  mv "$linked/.git" "$linked/moved.git" || fail "$name/git" "the .git directory is not movable"
  printf 'gitdir: moved.git\n' >"$linked/.git" || fail "$name/git" "the .git file is not writable"
  # 2. A repository with submodules declared: the hooks they carry would live under `.git/modules`.
  local submodules="$projects/submodules"
  mkdir -p "$submodules"
  git -C "$submodules" init --quiet || fail "$name/git" "git init did not work in $submodules"
  printf '[submodule "dep"]\n\tpath = dep\n\turl = ../dep\n' >"$submodules/.gitmodules" ||
    fail "$name/git" ".gitmodules is not writable"
  # 3. `extensions.worktreeConfig`, which would let the agent write a per-worktree config.
  local configured="$projects/configured"
  mkdir -p "$configured"
  git -C "$configured" init --quiet || fail "$name/git" "git init did not work in $configured"
  git -C "$configured" config extensions.worktreeConfig true ||
    fail "$name/git" "extensions.worktreeConfig is not settable"

  socket_pid="$(fake_docker_ping "$WORK/docker-refusal.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker-refusal.sock" ] && break
    sleep 0.05
  done
  # `min_free_gb = 1` because this machine's free space is not the case's subject, and `idle_stop_min = 0`
  # because this boot lasts a second and a sweeper would only be noise.
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-refusal.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$WORK" >"$WORK/config-refusal.toml"
  start_serve "$WORK/config-refusal.toml" "$WORK/data-refusal"

  for pair in "linked:git_not_directory" "submodules:submodules_present" \
    "configured:worktree_config_enabled"; do
    project="${pair%%:*}"
    code="${pair##*:}"
    fetch_body "${name}_$project" POST /api/sessions \
      "{\"project\":\"$project\",\"model\":\"claude-sonnet-4.5\",\"thinking\":\"high\"}" \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" \
      "Idempotency-Key: verify-refusal-$project" ||
      fail "${name}_$project/no_answer" "POST /api/sessions did not answer for $project"
    response="$(answer "${name}_$project")"
    expect_status "${name}_$project" "$response" 422
    expect_header "${name}_$project" "$response" "Content-Type" "application/json"
    body="$(cat "$WORK/responses/${name}_$project.body")"
    printf '%s\n' "$body" | grep -qF "\"code\":\"$code\"" ||
      fail "${name}_$project/code" "the answer is not $code: $body"
    assertions=$((assertions + 1))
  done

  # Nothing was written. The list is the artifact's own answer, so a refused create that had inserted a row
  # would show up here; the directory check is the fourth acceptance line's other half.
  fetch "${name}_list" GET /api/sessions "Host: 127.0.0.1" ||
    fail "${name}_list/no_answer" "GET /api/sessions did not answer"
  body="$(cat "$WORK/responses/${name}_list.body")"
  printf '%s\n' "$body" | grep -qF '"sessions":[]' ||
    fail "${name}_list/empty" "a refused create wrote a session: $body"
  assertions=$((assertions + 1))
  if [ -d "$projects/.worktrees" ]; then
    fail "${name}/worktrees" "a refused create made a worktree directory: $projects/.worktrees"
  fi
  assertions=$((assertions + 1))

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# HS-024's proof from the artifact: a machine that cannot run Docker sessions refuses *at startup*, with
# one sentence that names the fix, and binds nothing.
#
# The case points `docker_host` at a socket that cannot exist, so it is deterministic on every machine —
# including one with a running daemon, which is what a case that waited for "no docker here" would depend
# on. It boots nothing that serves, so it needs no `start_serve`: exit code, stderr, and no banner.
case_docker_preflight() {
  local name=case_docker_preflight status=0
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\ndocker_host = "unix://%s/absent.sock"\n' \
    "$owner" "$WORK" >"$WORK/config-docker.toml"
  "$artifact" serve --config "$WORK/config-docker.toml" --port 0 --data-dir "$WORK/data-docker" \
    >"$WORK/$name.out" 2>"$WORK/$name.err" || status=$?
  check "$name/exit" 1 "$status"
  if grep -q '^control_listen ' "$WORK/$name.err"; then
    fail "$name/banner" "a refused startup printed a banner: $(cat "$WORK/$name.err")"
  fi
  assertions=$((assertions + 1))
  # No migration either: the preflight runs before the database is opened, so a machine that cannot run
  # sessions does not get one (the review of this story found the order was the other way round).
  if [ -f "$WORK/data-docker/app.db" ]; then
    fail "$name/database" "a refused startup created the database: $WORK/data-docker"
  fi
  assertions=$((assertions + 1))
  # The sentence names the daemon, what went wrong and the two fixes. Asserted by its parts, because the
  # rule is "one sentence naming the fix" and not this wording.
  local line
  line="$(grep 'docker: the docker daemon at unix://' "$WORK/$name.err" || true)"
  [ -n "$line" ] ||
    fail "$name/sentence" "no sentence about the daemon: $(cat "$WORK/$name.err")"
  case "$line" in
    *"start it with"*"docker_host"*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/fix" "the sentence names no fix: $line"
      ;;
  esac
}

# HS-045's `--quick`, from the shipped artifact, on a machine that needs no daemon: six checks that are the
# preflight's own (`docker`, `git`, `disk` — and `merge`, V2's trial), and four doctor adds (`systemd`,
# `tailscale`, the two `ports`, `database`). The daemon is a `_ping` socket this case answers, so the docker
# line is deterministic on every machine — including one with a real daemon — which is `case_docker_preflight`'s
# determinism applied to a passing run. Three runs, each one a different fact:
#
#   1. a machine that passes: exit 0, one line per check, and the two `unknown` lines this machine must have
#      (no systemd, no `tailscaled`) — ADR-0054's rule that "not measured" is not "broken";
#   2. a held control port, bound by a python listener this case starts after the first run: `ports` fails
#      with the sentence that names what to stop, and the exit code is 1 — `ExecStartPre`'s whole point;
#   3. `docker_host` at a socket that cannot exist: the `docker` line carries **the same sentence** `serve`
#      refuses with (`case_docker_preflight` asserts that half) and the other checks still print, because a
#      machine missing one thing must be fixable in one round trip.
#
# `tests/doctor.rs` is where every check's arms are driven over shims, including the ones no gate machine can
# produce (a logged-out Tailscale, a git 2.37, an image running another pi).
# HS-047's `server deploy`, from the shipped artifact. Two halves that skip independently, because the bugs it
# exists to catch are of two kinds — the framework and the words, which need no daemon, and the install and the
# two proxies on a real one (ADR-0056).
#
# The **offline** half answers `GET /_ping` from a socket it starts, so step 1 passes on a machine with no
# daemon, and asserts four things: the dry run lists the seven steps in the spec's order with steps 3, 5, 6 and 7
# naming the story that still owns them, and leaves the install root **absent**; a run whose `docker_host` names a
# socket that cannot exist stops at step 1 and prints the resume command as its last line; a run that gets past
# step 1 and then fails step 4 — the same `_ping` socket answers the ping and nothing else — leaves step 2's files
# on disk, evaluates no later step at all, and prints the same runnable resume command.
#
# The **online** half needs the daemon and an image `agent_image` names (today a `docker tag` of the test image,
# because step 3 builds nothing until HS-048): a real install into a root under `target/` — step 2's four paths
# with their modes, step 4's two containers — and then the same command again, where steps 1, 2 and 4 say
# `satisfied`, the tree's listing and `config.toml`'s bytes are identical, and `keys.toml` was not rewritten. That
# is the acceptance's "the second run changes nothing", as a comparison rather than an absence of output.
#
# A run whose remaining steps are later stories' exits **0** with an `incomplete` line; only a failed step exits
# 1. `tests/deploy.rs` is where the words, the order, the modes and the `sudo` seam are driven over shims.

# The config a fresh host would have, written by the case rather than by the deploy: `data_root` is deliberately
# left at its default, because `--root` is what prefixes it and a file that already named the prefixed path would
# be prefixed twice (ADR-0056).
deploy_config() {
  local path="$1" control="$2" preview="$3" docker_host="$4" image="$5"
  cat >"$path" <<EOF
owner = "$owner"
role = "server"
backend = "stub"
control_listen = "127.0.0.1:$control"
preview_listen = "127.0.0.1:$preview"
docker_host = "$docker_host"
agent_image = "$image"
min_free_gb = 1
EOF
}

# Every entry under a directory, sorted, each with its mode and size: what "the second run changed nothing" is
# compared against. A listing rather than a hash so a failure names the entry that moved.
deploy_listing() {
  find "$1" -mindepth 1 2>/dev/null | sort | while IFS= read -r entry; do
    printf '%s %s\n' "$entry" "$(stat -c '%a %s' "$entry")"
  done
}

case_doctor_quick() {
  local name=case_doctor_quick root="$repo_root/target/verify-doctor-$$"
  local control preview status word line
  # The guard is first, and before anything else that could fail: python3 is optional on a hub-class machine.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to pick ports and answer `_ping` (tests/doctor.rs covers every check)\n' "$name"
    return 0
  fi
  rm -rf "$root"
  mkdir -p "$root/data"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  socket_pid="$(fake_docker_ping "$root/docker.sock")"
  for _ in $(seq 1 100); do
    [ -S "$root/docker.sock" ] && break
    sleep 0.05
  done
  [ -S "$root/docker.sock" ] || fail "$name/socket" "the fake daemon socket never appeared"

  # `role = "server"` because that is who runs the Docker and git checks (item 10 §6 omits them for a hub),
  # and `backend = "stub"` so nothing in this case can start a container.
  printf 'owner = "%s"\nrole = "server"\nbackend = "stub"\ncontrol_listen = "127.0.0.1:%s"\npreview_listen = "127.0.0.1:%s"\ndata_root = "%s"\ndocker_host = "unix://%s/docker.sock"\nmin_free_gb = 1\n' \
    "$owner" "$control" "$preview" "$root/data" "$root" >"$WORK/config-doctor-quick.toml"

  status=0
  "$artifact" doctor --quick --config "$WORK/config-doctor-quick.toml" \
    >"$WORK/$name.out" 2>"$WORK/$name.err" || status=$?
  [ "$status" -eq 0 ] ||
    fail "$name/exit" "doctor --quick exited $status on a machine that passes: $(cat "$WORK/$name.out" "$WORK/$name.err")"
  assertions=$((assertions + 1))
  grep -qx 'doctor: quick' "$WORK/$name.out" ||
    fail "$name/header" "the report does not name its depth: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # One line per check, in the spec's list. The `pass`/`unknown` split is asserted per check below where it is a
  # fact of this machine, and left to `tests/doctor.rs` where it is not (a systemd machine's `systemd: pass`).
  for word in systemd docker git merge disk tailscale ports database; do
    grep -q "^$word: \(pass\|unknown\): " "$WORK/$name.out" ||
      fail "$name/$word" "no $word line in the report: $(cat "$WORK/$name.out")"
    assertions=$((assertions + 1))
  done
  for word in docker git merge disk database; do
    grep -q "^$word: pass: " "$WORK/$name.out" ||
      fail "$name/$word/pass" "$word did not pass on this machine: $(cat "$WORK/$name.out")"
    assertions=$((assertions + 1))
  done
  # The two ports are the ones this case picked, so the sentence is asserted rather than the word alone.
  grep -q "^ports: pass: control 127.0.0.1:$control is free" "$WORK/$name.out" ||
    fail "$name/control" "the control port is not reported free: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  grep -q "^ports: pass: preview 127.0.0.1:$preview is free" "$WORK/$name.out" ||
    fail "$name/preview" "the preview port is not reported free: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # A machine that has never served has no database, which is a pass and not a finding.
  grep -q '^database: pass: .*no database' "$WORK/$name.out" ||
    fail "$name/database" "a fresh data root is not a pass: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # No failure anywhere: the tally is one line, and `unknown` is not a failure (ADR-0044/ADR-0054).
  grep -q '^doctor: [0-9]* findings: [0-9]* pass, 0 fail, ' "$WORK/$name.out" ||
    fail "$name/tally" "the tally is not zero failures: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # The honest lines: `serve` by hand is supported without either, so neither may be a failure here.
  line="$(grep '^systemd: ' "$WORK/$name.out" || true)"
  case "$line" in
    'systemd: pass: '* | 'systemd: unknown: '*"serve"*"server deploy"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/systemd" "the systemd line is neither a pass nor an honest unknown: $line" ;;
  esac
  line="$(grep '^tailscale: ' "$WORK/$name.out" || true)"
  case "$line" in
    'tailscale: pass: '*logged-in* | \
      'tailscale: unknown: '*did\ not\ answer* | \
      'tailscale: unknown: '*not\ runnable* | \
      'tailscale: fail: '*tailscale\ up*)
      assertions=$((assertions + 1))
      ;;
    *) fail "$name/tailscale" "the tailscale line does not say what it measured: $line" ;;
  esac

  # 2. A held control port, before a start: the failure `ExecStartPre` exists for.
  python3 - "$control" <<'PY' >/dev/null 2>&1 &
import socket, sys
server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", int(sys.argv[1])))
server.listen(4)
while True:
    connection, _ = server.accept()
    connection.recv(512)
    connection.sendall(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
    connection.close()
PY
  holder_pid=$!
  for _ in $(seq 1 100); do
    if (exec 3<>/dev/tcp/127.0.0.1/"$control") 2>/dev/null; then break; fi
    sleep 0.05
  done
  status=0
  "$artifact" doctor --quick --config "$WORK/config-doctor-quick.toml" \
    >"$WORK/$name.held.out" 2>&1 || status=$?
  check "$name/held/exit" 1 "$status"
  line="$(grep '^ports: fail: control ' "$WORK/$name.held.out" || true)"
  [ -n "$line" ] || fail "$name/held/line" "the held port is not a failure: $(cat "$WORK/$name.held.out")"
  case "$line" in
    *"in use"*"ss -ltnp"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/held/sentence" "the failure names no way to find the holder: $line" ;;
  esac
  kill "$holder_pid" 2>/dev/null || true
  wait "$holder_pid" 2>/dev/null || true
  holder_pid=""

  # 3. A daemon that is not there: one failure, its sentence, and the rest of the list still printed.
  printf 'owner = "%s"\nrole = "server"\nbackend = "docker"\ndocker_host = "unix://%s/absent.sock"\ndata_root = "%s"\nmin_free_gb = 1\n' \
    "$owner" "$root" "$root/data" >"$WORK/config-doctor-absent.toml"
  status=0
  "$artifact" doctor --quick --config "$WORK/config-doctor-absent.toml" \
    >"$WORK/$name.absent.out" 2>&1 || status=$?
  check "$name/absent/exit" 1 "$status"
  line="$(grep '^docker: fail: ' "$WORK/$name.absent.out" || true)"
  [ -n "$line" ] || fail "$name/absent/line" "a daemon that cannot answer is not a failure: $(cat "$WORK/$name.absent.out")"
  case "$line" in
    *"start it with"*"docker_host"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/absent/sentence" "the docker failure names no fix: $line" ;;
  esac
  for line in git merge disk ports database; do
    grep -q "^$line: " "$WORK/$name.absent.out" ||
      fail "$name/absent/$line" "one failure hid the $line line: $(cat "$WORK/$name.absent.out")"
    assertions=$((assertions + 1))
  done

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
  rm -rf "$root"
}

case_server_deploy() {
  local name=case_server_deploy root="$repo_root/target/verify-deploy-$$"
  local image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local control preview status line got entry n

  # ---- the offline half: step 1 against a `_ping` socket, and the two failures ----------------------
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s/offline: no python3 to answer `_ping` (tests/deploy.rs covers the words, the order and the sudo seam)\n' "$name"
  else
    rm -rf "$root"
    mkdir -p "$root"
    control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
    preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
    socket_pid="$(fake_docker_ping "$root/docker.sock")"
    for _ in $(seq 1 100); do
      [ -S "$root/docker.sock" ] && break
      sleep 0.05
    done
    [ -S "$root/docker.sock" ] || fail "$name/socket" "the fake daemon socket never appeared"
    deploy_config "$WORK/config-deploy-offline.toml" "$control" "$preview" "unix://$root/docker.sock" "$image"

    # 1. The dry run: the seven steps in the spec's order, and an install root that does not appear.
    status=0
    "$artifact" server deploy --dry-run --root "$root/install" \
      --config "$WORK/config-deploy-offline.toml" >"$WORK/$name.dry.out" 2>&1 || status=$?
    check "$name/dry/exit" 0 "$status"
    got="$(sed -n 's/^\([1-7]\)\. .*/\1/p' "$WORK/$name.dry.out" | tr -d '\n')"
    check "$name/dry/order" "1234567" "$got"
    for entry in preflight files images proxies service origins health; do
      grep -q "^[1-7]\. $entry: \(needs\|satisfied\|skipped\|failed\): " "$WORK/$name.dry.out" ||
        fail "$name/dry/$entry" "no $entry line in the dry run: $(cat "$WORK/$name.dry.out")"
      assertions=$((assertions + 1))
    done
    grep -q '^2\. files: needs: ' "$WORK/$name.dry.out" ||
      fail "$name/dry/plan" "the dry run does not say what step 2 would do: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    # A step another story owns is a skip that names it, never a pass (ADR-0056).
    while read -r n entry line; do
      grep -q "^$n\. $entry: skipped: .*$line" "$WORK/$name.dry.out" ||
        fail "$name/dry/skip$n" "step $n is not a skip naming $line: $(cat "$WORK/$name.dry.out")"
      assertions=$((assertions + 1))
    done <<'OWNERS'
3 images HS-048
5 service HS-049
6 origins HS-049
7 health HS-049
OWNERS
    [ ! -e "$root/install" ] || fail "$name/dry/root" "the dry run created $root/install"
    assertions=$((assertions + 1))
    grep -q '^deploy: 7 steps: ' "$WORK/$name.dry.out" ||
      fail "$name/dry/summary" "no summary line: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    grep -q '^deploy: incomplete: 4 of 7 steps' "$WORK/$name.dry.out" ||
      fail "$name/dry/incomplete" "the summary does not say four steps are still other stories': $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    grep -q '^deploy: url: http' "$WORK/$name.dry.out" ||
      fail "$name/dry/url" "no URL in the tail: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    grep -q '^deploy: next: holosmith key set <provider>' "$WORK/$name.dry.out" ||
      fail "$name/dry/next" "the next steps name no key command: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))

    # 2. A daemon that cannot be there: step 1 fails, nothing after it is evaluated, and the last line is the
    #    resume command — the same command, with the resolved paths, and without `--dry-run`.
    deploy_config "$WORK/config-deploy-absent.toml" "$control" "$preview" "unix://$root/absent.sock" "$image"
    status=0
    "$artifact" server deploy --root "$root/absent-install" \
      --config "$WORK/config-deploy-absent.toml" >"$WORK/$name.absent.out" 2>&1 || status=$?
    check "$name/absent/exit" 1 "$status"
    grep -q '^1\. preflight: failed: .*absent\.sock' "$WORK/$name.absent.out" ||
      fail "$name/absent/step1" "step 1 does not name the daemon: $(cat "$WORK/$name.absent.out")"
    assertions=$((assertions + 1))
    if grep -q '^2\. ' "$WORK/$name.absent.out"; then
      fail "$name/absent/stop" "a run that failed at step 1 evaluated step 2: $(cat "$WORK/$name.absent.out")"
    fi
    assertions=$((assertions + 1))
    line="$(tail -n 1 "$WORK/$name.absent.out")"
    case "$line" in
      'deploy: resume: holosmith server deploy --config '*config-deploy-absent.toml*' --root '*absent-install) assertions=$((assertions + 1)) ;;
      *) fail "$name/absent/resume" "the last line is not the runnable resume command: $line" ;;
    esac
    case "$line" in
      *--dry-run*) fail "$name/absent/resume/dry" "the resume command carries --dry-run: $line" ;;
      *) assertions=$((assertions + 1)) ;;
    esac

    # 3. A run that gets past step 1 and then fails step 4: that same socket answers `_ping` and nothing else,
    #    so step 2's files are on disk when the run stops and the machine is describable.
    status=0
    "$artifact" server deploy --root "$root/partial" \
      --config "$WORK/config-deploy-offline.toml" >"$WORK/$name.partial.out" 2>&1 || status=$?
    check "$name/partial/exit" 1 "$status"
    grep -q '^1\. preflight: satisfied: ' "$WORK/$name.partial.out" ||
      fail "$name/partial/step1" "step 1 did not pass: $(cat "$WORK/$name.partial.out")"
    assertions=$((assertions + 1))
    grep -q '^2\. files: changed: ' "$WORK/$name.partial.out" ||
      fail "$name/partial/step2" "step 2 did not create the files: $(cat "$WORK/$name.partial.out")"
    assertions=$((assertions + 1))
    grep -q '^3\. images: skipped: ' "$WORK/$name.partial.out" ||
      fail "$name/partial/step3" "step 3 is not a skip: $(cat "$WORK/$name.partial.out")"
    assertions=$((assertions + 1))
    grep -q '^4\. proxies: failed: ' "$WORK/$name.partial.out" ||
      fail "$name/partial/step4" "step 4 did not fail against a `_ping` socket: $(cat "$WORK/$name.partial.out")"
    assertions=$((assertions + 1))
    if grep -q '^5\. ' "$WORK/$name.partial.out"; then
      fail "$name/partial/stop" "a run that failed at step 4 evaluated step 5: $(cat "$WORK/$name.partial.out")"
    fi
    assertions=$((assertions + 1))
    [ -f "$root/partial/etc/holosmith/config.toml" ] ||
      fail "$name/partial/left" "the failed run left no config.toml, so the machine is not describable"
    assertions=$((assertions + 1))
    [ -d "$root/partial/var/lib/holosmith/secrets" ] ||
      fail "$name/partial/left/secrets" "the failed run left no secrets directory"
    assertions=$((assertions + 1))
    line="$(tail -n 1 "$WORK/$name.partial.out")"
    case "$line" in
      'deploy: resume: holosmith server deploy --config '*"--root $root/partial") assertions=$((assertions + 1)) ;;
      *) fail "$name/partial/resume" "the last line is not the runnable resume command: $line" ;;
    esac
    if grep -q '^deploy: url: ' "$WORK/$name.partial.out"; then
      fail "$name/partial/url" "a failed run printed the URL as though it had installed something"
    fi
    assertions=$((assertions + 1))
    kill "$socket_pid" 2>/dev/null || true
    wait "$socket_pid" 2>/dev/null || true
    socket_pid=""
  fi

  # ---- the online half: a real install, and then the same command again ----------------------------
  if ! command -v docker >/dev/null 2>&1 || ! docker version >/dev/null 2>&1 ||
    ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s/online: no docker daemon or no %s image (step 3 builds one in HS-048; tests/deploy.rs covers the rest)\n' \
      "$name" "$image"
    rm -rf "$root"
    return 0
  fi
  # `hs_egress`/`hs_key` are the machine's names: a leftover from an earlier case would make this case's first
  # run read `satisfied`, so it takes them over and removes its own at the end.
  timeout 60 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  rm -rf "$root"
  mkdir -p "$root"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  deploy_config "$WORK/config-deploy-online.toml" "$control" "$preview" "unix:///var/run/docker.sock" "$image"

  status=0
  "$artifact" server deploy --root "$root/install" \
    --config "$WORK/config-deploy-online.toml" >"$WORK/$name.online.out" 2>&1 || status=$?
  check "$name/online/exit" 0 "$status"
  grep -q '^2\. files: changed: ' "$WORK/$name.online.out" ||
    fail "$name/online/step2" "step 2 did not create the files: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  grep -q '^4\. proxies: changed: ' "$WORK/$name.online.out" ||
    fail "$name/online/step4" "step 4 did not start the proxies: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  check "$name/online/config/mode" 644 "$(stat -c '%a' "$root/install/etc/holosmith/config.toml")"
  check "$name/online/data/mode" 755 "$(stat -c '%a' "$root/install/var/lib/holosmith")"
  check "$name/online/secrets/mode" 700 "$(stat -c '%a' "$root/install/var/lib/holosmith/secrets")"
  check "$name/online/keys/mode" 600 "$(stat -c '%a' "$root/install/var/lib/holosmith/secrets/keys.toml")"
  check "$name/online/admin/mode" 700 "$(stat -c '%a' "$root/install/run/holosmith")"
  # The file it wrote is the machine it measured: the prefixed data root, and a file that re-reads clean.
  grep -q "^data_root = \"$root/install/var/lib/holosmith\"$" "$root/install/etc/holosmith/config.toml" ||
    fail "$name/online/config/data_root" "the written config does not name the prefixed data root: $(cat "$root/install/etc/holosmith/config.toml")"
  assertions=$((assertions + 1))
  status=0
  "$artifact" config validate --config "$root/install/etc/holosmith/config.toml" >/dev/null 2>&1 || status=$?
  check "$name/online/config/valid" 0 "$status"
  docker ps --format '{{.Names}}' | grep -qx hs_egress ||
    fail "$name/online/egress" "hs_egress is not running"
  assertions=$((assertions + 1))
  docker ps --format '{{.Names}}' | grep -qx hs_key || fail "$name/online/key" "hs_key is not running"
  assertions=$((assertions + 1))
  docker network ls | grep -qw egress || fail "$name/online/network" "the egress network is not there"
  assertions=$((assertions + 1))
  grep -q '^deploy: url: http' "$WORK/$name.online.out" ||
    fail "$name/online/url" "a completed run printed no URL: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))

  # The second run: the acceptance's "changes nothing", as a comparison of the tree, the config's bytes and the
  # key file the deploy must never rewrite.
  deploy_listing "$root/install" >"$WORK/$name.tree1"
  cp "$root/install/etc/holosmith/config.toml" "$WORK/$name.config1"
  cp "$root/install/var/lib/holosmith/secrets/keys.toml" "$WORK/$name.keys1"
  status=0
  "$artifact" server deploy --root "$root/install" \
    --config "$WORK/config-deploy-online.toml" >"$WORK/$name.again.out" 2>&1 || status=$?
  check "$name/again/exit" 0 "$status"
  while read -r n entry; do
    grep -q "^$n\. $entry: satisfied: " "$WORK/$name.again.out" ||
      fail "$name/again/$n" "step $n is not satisfied on the second run: $(cat "$WORK/$name.again.out")"
    assertions=$((assertions + 1))
  done <<'AGAIN'
1 preflight
2 files
4 proxies
AGAIN
  deploy_listing "$root/install" >"$WORK/$name.tree2"
  cmp -s "$WORK/$name.tree1" "$WORK/$name.tree2" ||
    fail "$name/again/tree" "the second run changed the tree: $(diff "$WORK/$name.tree1" "$WORK/$name.tree2" 2>&1 | head -5)"
  assertions=$((assertions + 1))
  cmp -s "$WORK/$name.config1" "$root/install/etc/holosmith/config.toml" ||
    fail "$name/again/config" "the second run rewrote config.toml"
  assertions=$((assertions + 1))
  cmp -s "$WORK/$name.keys1" "$root/install/var/lib/holosmith/secrets/keys.toml" ||
    fail "$name/again/keys" "the second run rewrote keys.toml, which holds every stored provider key"
  assertions=$((assertions + 1))
  grep -q '^deploy: incomplete: 4 of 7 steps' "$WORK/$name.again.out" ||
    fail "$name/again/incomplete" "the second run does not say four steps are still other stories': $(cat "$WORK/$name.again.out")"
  assertions=$((assertions + 1))
  timeout 60 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  rm -rf "$root"
}

# HS-045's `--full` from the shipped artifact, at **item 10 §7's moment**: the machine is already serving, which
# is what makes the four additions meaningful — the self-test has a daemon to run in, both proxies exist because
# `serve`'s boot started them, the ports are held by this machine's own server (the pass `--quick` fails on), and
# the image's pi version is a fact about the image the config names.
#
# It stops the shared boot and leaves its own running, which is the shape `case_reconcile_orphans` and
# `case_preview` established: the phase's final `stop_serve` is what ends the last server. The proxies outlive it,
# as they outlive every server (`serve` starts them; nothing stops them), so they are read here and not removed —
# `tests/container.rs` is the one test that minds, and it skips rather than replacing a live proxy.
#
# A machine without the daemon or the image prints one SKIP line, and `tests/doctor.rs` is where the decision is
# proven for the machines this one cannot be: the arms its fake `Machine` produces are the ones a skipped run
# would otherwise hide.
case_doctor_full() {
  local name=case_doctor_full image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  # Two `local`s, not one: `set -u` expands `$root` before `local` assigns it (HS-038's landmine, hit again).
  local root="$repo_root/target/verify-doctor-full-$$"
  local data="$root/data" control preview status=0 pinned networks
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/doctor.rs drives the same checks over a fake machine)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to pick the two ports doctor must find held (tests/doctor.rs covers the arms)\n' "$name"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$data" "$root/projects"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  # The scratch lives under `target/` for ADR-0035's reason: the daemon resolves a mount's source in the host's
  # namespace, and the proxies mount this binary at its own path. **`data_root` and the two listeners are in the
  # file**, because `doctor` reads the config and nothing else — `--data-dir` and `--port` are `serve` overrides
  # (a `doctor` run given only the file would otherwise check `/var/lib/holosmith` and port 8080).
  printf 'owner = "%s"\nrole = "server"\nbackend = "docker"\nmax_live_sessions = 1\nprojects_root = "%s"\nagent_image = "%s"\ndata_root = "%s"\ncontrol_listen = "127.0.0.1:%s"\npreview_listen = "127.0.0.1:%s"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$root/projects" "$image" "$data" "$control" "$preview" >"$WORK/config-doctor-full.toml"
  # Its own boot, **on the config's own ports**: `start_serve` always passes `--port 0`, which would leave the
  # config naming a port nothing listens on and `doctor`'s port check asking about the wrong address. The banner
  # is what says serve is up (the proxies and the boot's self-test run before it), and the loop gives the
  # self-test's container the half minute it can take.
  serve_err="$WORK/$name.serve.err"
  "$artifact" serve --config "$WORK/config-doctor-full.toml" --data-dir "$data" \
    >"$WORK/$name.serve.out" 2>"$serve_err" &
  serve_pid=$!
  for _ in $(seq 1 600); do
    grep -q '^control_listen ' "$serve_err" 2>/dev/null && break
    kill -0 "$serve_pid" 2>/dev/null || break
    sleep 0.05
  done
  grep -q "^control_listen 127.0.0.1:$control\$" "$serve_err" ||
    fail "$name/serve" "serve did not bind the config's control port: $(cat "$serve_err")"
  assertions=$((assertions + 1))

  "$artifact" doctor --full --config "$WORK/config-doctor-full.toml" \
    >"$WORK/$name.out" 2>"$WORK/$name.err" || status=$?
  [ "$status" -eq 0 ] ||
    fail "$name/exit" "doctor --full exited $status on a serving machine: $(cat "$WORK/$name.out" "$WORK/$name.err")"
  assertions=$((assertions + 1))
  grep -qx 'doctor: full' "$WORK/$name.out" ||
    fail "$name/header" "the report does not name its depth: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # The four additions, each one line. `network` is the self-test's outcome, and its bare probe *is* the
  # network-off probe (ADR-0044/ADR-0046); `proxy` is two lines because `hs_egress` and `hs_key` are two facts.
  for check in network image; do
    grep -q "^$check: pass: " "$WORK/$name.out" ||
      fail "$name/$check" "$check did not pass while the machine serves: $(cat "$WORK/$name.out")"
    assertions=$((assertions + 1))
  done
  grep -q '^proxy: pass: hs_egress is running' "$WORK/$name.out" ||
    fail "$name/egress" "hs_egress is not reported up: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  grep -q '^proxy: pass: hs_key is running' "$WORK/$name.out" ||
    fail "$name/key" "hs_key is not reported up: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # The image's pi against `pins/pi.txt`, the one copy of that version: the line names it, and the case reads the
  # pin rather than repeating it.
  pinned="$(cat "$repo_root/pins/pi.txt")"
  grep -q "^image: pass: .*pi $pinned" "$WORK/$name.out" ||
    fail "$name/image" "the image pass does not name the pinned pi: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # Item 10 §7's moment for the ports: they are held, and that is the machine serving rather than a failure.
  grep -q '^ports: pass: control 127.0.0.1:[0-9]* is held by this machine' "$WORK/$name.out" ||
    fail "$name/control" "the serving control port is not a pass: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  grep -q '^ports: pass: preview 127.0.0.1:[0-9]* is held by this machine' "$WORK/$name.out" ||
    fail "$name/preview" "the serving preview port is not a pass: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  grep -q '^doctor: [0-9]* findings: [0-9]* pass, 0 fail, ' "$WORK/$name.out" ||
    fail "$name/tally" "the tally is not zero failures: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # A `doctor` leaves nothing behind: the self-test's network and probe container are its own (ADR-0044), and a
  # network left over is the name the next boot has to work around.
  networks="$(docker network ls --format '{{.Name}}' 2>/dev/null || true)"
  if printf '%s\n' "$networks" | grep -qx 'holosmith-selftest'; then
    fail "$name/network_left" "the self-test's network outlived doctor"
  fi
  assertions=$((assertions + 1))
}

# Box 4, over every answer the cases above saved: none of them carries a CORS header, and none
# carries a `Vary` that would imply one. Runs after the case loop rather than as a member of it,
# so nothing answered above is missed and a case appended later is covered automatically.
case_no_cors_header() {
  local file label
  for file in "$WORK"/responses/*.http; do
    label="$(basename "$file" .http)"
    # The header block alone, read from the file rather than the whole answer: `expect_no_cors` pipes its
    # argument through `sed '/^\r$/q'`, which quits at the blank line, so a body of any size in a shell
    # variable leaves the writer with SIGPIPE — and `set -o pipefail` makes that exit the run at 141 instead
    # of failing an assertion (measured, with HS-033's two-megabyte tool output).
    expect_no_cors "$label" "$(sed '/^\r$/q' "$file")"
  done
}

# start_serve [config] [data dir]: boot the artifact and read the values the cases need from its
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
  # The preview listener's port too (HS-041): the config asks for `127.0.0.1:0`, so this is the only place it can
  # be read — and a boot whose preview could not bind is a fact this run has to know, not guess.
  preview_listen="$(sed -n 's/^preview_listen //p' "$serve_err" | sed -n '1p')"
  case "$preview_listen" in
    127.0.0.1:[0-9]*) preview_port="${preview_listen##*:}" ;;
    *)
      fail "serve/banner" "the preview listener is not bound, and this run's cases need it: $preview_listen"
      ;;
  esac
  printf 'ok serve/banner: listening on %s, control origin %s, preview %s\n' \
    "$control_listen" "$control_origin" "$preview_listen"
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
  # `preview_listen` is asked for the kernel's port, like `--port 0` for the control listener: the case that
  # exercises the preview listener reads the real port from the banner, and two boots on this machine (a case's own
  # and this one) then cannot collide over 8081.
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\nidle_stop_min = 1\nturn_timeout_min = 1\npreview_listen = "127.0.0.1:0"\n' "$owner"   \
    >"$WORK/config.toml"

  start_serve
  local case_fn case_out skipped=0
  for case_fn in "${serve_cases[@]}"; do
    # A case's own standard output is captured so the loop can tell a case that measured something from one
    # that printed SKIP and returned. An `ok` for a case that skipped would log a pass for a check that never
    # ran, which is the one shape this gate may not have (HS-046's review); nothing is swallowed, the lines are
    # printed below in order whatever they are.
    case_out="$WORK/case.out"
    "$case_fn" >"$case_out"
    cat "$case_out"
    if grep -q '^SKIP' "$case_out"; then
      skipped=$((skipped + 1))
    else
      # One line per case that measured something, so a green run names what it proved and a reader can point
      # at the case a verdict in HolosmithSpec.md's verification table rests on (HS-046).
      printf 'ok %s\n' "$case_fn"
    fi
  done
  if [ "$skipped" -gt 0 ]; then
    printf 'SKIP %d of %d cases measured nothing: their `ok` line is withheld, and the SKIP lines above say why\n' \
      "$skipped" "${#serve_cases[@]}"
  fi
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
# HS-075: the expand/contract rule in one named command. The destructive side is refused where a migration is
# written (`schema::contract_violations`, which `cargo test` already runs); this is the other half — the previous
# release's schema checked against this build's — given a phase of its own so a migration author sees the rule
# named in the output instead of buried in a test list (ADR-0047).
run_phase migrations cargo test --test schema_previous
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
