#!/usr/bin/env bash
# The one command that proves this tree builds, that its code passes, and that the artifact it
# ships serves (ADR-0003).
#
# Usage: scripts/verify.sh [--keep] [--no-build]
# Seven phases, cheapest first: cargo fmt --check, cargo clippy --all-targets -- -D warnings,
# cargo test, `cargo test --test schema_previous` (the expand/contract rule),
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
# release (which continues and says who refuses), the service and the keys a deploy writes in a case of their own
# straight after `case_server_deploy` (`case_server_service`: the unit's bytes, the VAPID pair, the printed commands
# that are root's and `doctor`'s `perms` arms), the three service commands (`case_status`: one line per fact, with
# the uptime read from the endpoint the boot answers; `case_logs`: the journald child's argv through a stand-in and a
# session's log printed byte for byte; `case_uninstall`: the unit and the two proxies gone, the data kept, a second
# run saying `nothing`, and a purge refused over a live row until `--force`), the drain in two cases
# (`case_drain`: the admin socket exists and is not an HTTP route, a wait shorter than a turn exits 1 and leaves
# the drain on, the create and the prompt answer 503 `draining` while the reads answer 200, and the drain that
# does finish stops the idle sessions with reason `server_restart`; `case_drain_restart`: the row an expired drain
# and a `kill -9` left `running` comes back `idle · interrupted`, and the new process is not draining), the
# release build in a case of its own (`case_build`: the orchestration measured through a `docker` stand-in
# whose artifact is this tree's own binary and whose `cargo`/`rustc` stand-ins must never fire, plus the real
# container build when this machine already has the builder image), the smoke run in three arms and with
# no skip (`case_smoke`: the artifact's own `container fake-pi` over two pipes, a `docker` stand-in that refuses the
# smoke image and must be exit 1 with the smoke config on disk, and the real run — a session whose turn is asserted,
# then deleted, with the machine's two proxies compared by id before and after), the promotion in four arms
# (`case_upgrade`: the review gate and an older-schema candidate refused before anything is asked, then a real
# promotion over a `systemctl` stand-in that starts what `current` names and a `docker` stand-in that fakes only the two
# product images — the step order, the images built from the candidate's digest once and never again, the backup, a
# session the drain stopped coming back idle, and the last line naming the version and the backup — and a rollback whose
# first restart serves nothing, so the poll gives up and the symlink goes back), the rollback verb in four arms
# (`case_rollback`: a tree with no `current`, one with a single release and a `--to` that names nothing refused before the
# machine is asked anything, a database from a newer release refused before the drain with the link still where it was,
# then a real rollback over a `systemctl` stand-in that records what `current` names as it restarts — the switch before
# the restart, one restart, the session the drain stopped `idle` again, and a last arm whose restart serves nothing, so
# the poll gives up, the sentence names the journal and the way back forward, and the link is **not** moved again; it
# needs no Docker at all, because a rollback runs no smoke test and builds no image), and a Docker preflight that
# refuses to start with one sentence naming the fix, with no CORS header
# on any answer, and stopping on SIGTERM with exit 0. Each case prints `ok <case>` once it passes — a
# case that printed SKIP withholds that line, so the log can never call a check that measured nothing a
# pass — which makes the run's own output say which check each verdict in that table rests on.
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
# The preview listener's bound port, read from the banner: the config asks for port 0.
preview_listen=""
preview_port=""
assertions=0

# The only cleanup path: the work dir is removed on success, kept on failure (and with --keep),
# and a server a failure left running is stopped here rather than outliving the run.
# The session the running Docker case made, if any: the cases that start a real container set this, so a `fail`
# in the middle of one does not leave a live container and network on the machine (the review — the EXIT
# trap is the only hook a `fail` leaves, because `fail` exits the script). A case that makes **two** sessions
#  sets the second one too: one id cannot describe a pair, and a half-cleaned failure leaves exactly
# the orphan the trap exists to prevent.
docker_session_id=""
docker_second_session_id=""
# A case that runs a **process of its own** sets this, so `fail` — which exits the script, leaving the EXIT trap as
# the only hook — takes it down too (the push receiver is the first: a `node` listener on a port and a data
# root, which outlived two failing runs of `case_push` before the trap learned about it).
extra_pid=""
# The second machine a case runs. Its own pid, for `start_peer`'s reason.
peer_pid=""

cleanup() {
  local status=$?
  if [ -n "$serve_pid" ] && kill -0 "$serve_pid" 2>/dev/null; then
    kill -TERM "$serve_pid" 2>/dev/null || true
    wait "$serve_pid" 2>/dev/null || true
  fi
  # A case's second machine: two boots on one box, and the trap has to know about both or a `fail`
  # between them leaves a listener on a pinned port for the next run.
  if [ -n "$peer_pid" ] && kill -0 "$peer_pid" 2>/dev/null; then
    kill -TERM "$peer_pid" 2>/dev/null || true
    wait "$peer_pid" 2>/dev/null || true
  fi
  peer_pid=""
  if [ -n "$socket_pid" ] && kill -0 "$socket_pid" 2>/dev/null; then
    kill "$socket_pid" 2>/dev/null || true
    wait "$socket_pid" 2>/dev/null || true
  fi
  # A case that holds a port on purpose (the `doctor`, whose `ports` check is the one thing it can break
  # without touching the machine) leaves its holder here, so a `fail` in the middle does not strand a listener.
  if [ -n "$holder_pid" ] && kill -0 "$holder_pid" 2>/dev/null; then
    kill "$holder_pid" 2>/dev/null || true
    wait "$holder_pid" 2>/dev/null || true
  fi
  # A **smoke run's** own names. The harness removes its container, its network and its two proxies
  # itself; this is what a `fail` in the middle needs, and the filter is `smoke_` and nothing else — the
  # machine's own `hs_egress`/`hs_key` are not this case's to remove (the block above is).
  local leftover
  for leftover in $(docker ps -aq --filter 'name=smoke_' 2>/dev/null || true); do
    docker rm --force "$leftover" >/dev/null 2>&1 || true
  done
  for leftover in $(docker network ls -q --filter 'name=smoke_' 2>/dev/null || true); do
    docker network rm "$leftover" >/dev/null 2>&1 || true
  done
  # And a case's own process, for the same reason (`case_push`'s receiver).
  if [ -n "$extra_pid" ] && kill -0 "$extra_pid" 2>/dev/null; then
    kill "$extra_pid" 2>/dev/null || true
    wait "$extra_pid" 2>/dev/null || true
  fi
  extra_pid=""
  local id
  for id in ${docker_session_id:-} ${docker_second_session_id:-}; do
    docker rm --force "s_$id" >/dev/null 2>&1 || true
    # The egress proxy is an endpoint on a session network that allowed internet access, and `docker network rm`
    # refuses a network with one — so a teardown that forgot the detach would leave the network behind with a
    # `warn`, which is the orphan a later change owns. `docker.rs`'s `remove_network` leaves first for the same reason.
    docker network disconnect --force "agents_$id" hs_egress >/dev/null 2>&1 || true
    docker network rm "agents_$id" >/dev/null 2>&1 || true
  done
  docker_session_id=""
  docker_second_session_id=""
  # the image case starts the two proxies from the image it built — its own `hs_images_*` pair and the deploy's
  # `hs_egress`/`hs_key` — so a `fail` in the middle does not leave a container the next run's `--name` would trip
  # over (and a leftover `hs_egress` would make the *next* run's untag fail, which is how the absent state stops
  # being absent). Its images stay: every later `serve` boot starts the proxies from them.
  local image_container
  for image_container in $(docker ps -aq --filter 'name=hs_images_' --filter 'name=hs_egress' --filter 'name=hs_key' 2>/dev/null || true); do
    docker rm --force "$image_container" >/dev/null 2>&1 || true
  done
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
#
# The header block is cut with bash's own suffix removal rather than `sed '/^\r$/q'`, and the reason is
# measured: a `sed …q` closes the pipe at the blank line while `printf` is still writing, and under
# `pipefail` that SIGPIPE is a 141 that kills the whole run — which is exactly what a 256 KB answer did. Bash's
# `${var%%pattern}` cannot be interrupted.
expect_no_cors() {
  local name="$1" response="$2" headers
  headers="${response%%$'\r\n\r\n'*}"
  headers="$(printf '%s\n' "$headers" | tr -d '\r')"
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

# The same `/dev/tcp` fetch as `fetch`, against the **preview** listener rather than the control one.
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

# forward_fetch <port> <label> <method> <path> [Header: value]...: one request to a **named** machine's control
# listener. `fetch` always talks to `$port`, and a case that runs two machines has to address both: the hub
# through `fetch`, the server it forwards to through this.
forward_fetch() {
  local target="$1" label="$2" method="$3" path="$4"
  shift 4
  local -a sent=("$@")
  local request="$method $path HTTP/1.1"$'\r\n' fd header
  for header in "${sent[@]}"; do
    request+="$header"$'\r\n'
  done
  request+=$'Connection: close\r\n\r\n'
  exec {fd}<>"/dev/tcp/127.0.0.1/$target" || return 1
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
# reads is truncated — and the log grows with every turn this run takes: by the abort and steer
# cases the shared session has had four or five turns, which is 300-odd lines, so a budget sized for the
# log as it was at would end the read before the frame the case came for. A stream that has not
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
  local name="$1" response="$2" header="$3" expected="$4" value headers
  # The header block alone, for `expect_no_cors`'s reason: a `sed '/^\r$/q'` on a large answer is a SIGPIPE
  # under `pipefail` (measured), so the cut is bash's own.
  headers="${response%%$'\r\n\r\n'*}"
  value="$(printf '%s\n' "$headers" | tr -d '\r' |
    { grep -i "^${header}:" || true; } | sed -n '1s/^[^:]*: *//p')"
  check "$name/$header" "$expected" "$value"
}

# One step's block from a deploy report: its own line, then the indented action lines under it, up to the blank
# line that separates the next step.
#
# The step's first line carries the count and the word; **what the step would do is on the lines below it**
# (ADR-0085), so a check on a plan's content reads the block rather than the line. The header is an awk regex, so
# the caller writes the dots escaped (`'^5\. service: needs: '`).
#
# `|| true` is the `grep … || true` this replaced: under `set -e` a report that was never written would otherwise
# end the run with awk's status instead of the case's own `fail` sentence.
deploy_block() {
  awk -v header="$2" '$0 ~ header { inside = 1 } inside && !/^$/ { print; next } inside { exit }' "$1" || true
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
  case_stopped_expiry
  case_push
  case_pwa
  case_hosts
  case_forward
  case_machine_trust
  case_computers
  case_cross_machine
  case_hub_deploy
  case_idle_stop
  case_drain
  case_drain_restart
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
  case_changes
  case_changes_diff
  case_changes_screen
  case_merge
  case_merge_strategies
  case_merge_guards
  case_publish
  case_docker_preflight
  case_doctor_quick
  case_disk_floor
  case_disk_quota
  case_server_images
  case_build
  case_smoke
  case_upgrade
  case_rollback
  case_releases
  case_server_deploy
  case_server_service
  case_deploy_ssh
  case_status
  case_logs
  case_uninstall
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

# V1: the hardened `.git` mounts, measured on a real daemon. Three writes must fail, three git
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

# the proof from the artifact: a Docker boot runs the session's network and its container, and the row
# therefore ends `error` at the **agent** step rather than at the network or the container one.
#
# The container's own flags are `tests/container.rs`'s (it inspects a real container, which the API cannot
# show: the provisioner takes the container back the moment the agent step fails, exactly as ADR-0015 says).
# What this case proves is the *order* — two steps that go to a real daemon succeeded — and the teardown: no
# container, no network and no worktree are left behind, and the registration list is back to one entry.
#
# Its projects root lives under `target/` because the daemon resolves bind-mount sources in the host's
# namespace and `$WORK` is on `/tmp` (ADR-0035's measurement).
# the proof from the artifact: a Docker boot runs the session's network, its container **and the
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
# sources in the host's namespace and `$WORK` is on `/tmp` (ADR-0035): a data root the daemon
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

# the proof from the artifact, now over a live agent: a session reaches `idle`, the container dies from
# under it (the row is `error`, the exit classified), and `POST /resume` brings it back to `idle` with the same
# worktree and a new container. The resume's container step is the path that clears the expected-stop flag
#  and the launch that replaces an exit nobody is waiting for.
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
  # would let the teardown run against paths that are already gone and prove nothing.
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
  # backend did not forget the launch first (ADR-0039).
  [ ! -d "$data/sessions/$id" ] ||
    fail "${name}/session_dir_left" "the delete left (or re-created) $data/sessions/$id"
  assertions=$((assertions + 1))
  docker_session_id=""
  # Its server is left running: `phase_serve` stops whatever is up after the last case.
  rm -rf "$root"
}

# the proof from the artifact: a prompt on a Docker session reaches **pi's RPC loop** and what pi answers
# comes back as the row's own reason. The mapping itself (a record in, a frame out) is `cargo test --test rpc`'s,
# against records captured from a real turn; this case is the half only a real container can show.
#
# The assertion is a refusal and not a streamed answer, and that is a fact about this phase rather than a
# shortcut: a session's container is on an `--internal` network and holds no provider key, so
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

# the real half: a session with a commit on `agent/<id>` and an uncommitted change, deleted from the
# shipped artifact against a real repository. The branch and the snapshot ref survive, the worktree and the
# session directory do not, and the command the 202 carried recovers the work — the story's demo, as a gate.
#
# It needs the daemon and `agent_image` for the session to reach `idle` (a create provisions a real
# container), so a machine without them prints one SKIP line; the keep-rule is still proven there by
# `tests/worktree.rs`, which needs no daemon at all. The cleanup is explicit rather than a trap because a
# gate case that fails should still not leave the next run a container to trip over.
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

# The health payload: exactly seven keys (the sixth is the application server key, the seventh is that story's
# machine name), and the uptime is not predictable, so the assertion is the prefix and the closing brace (the shape
# tests/http_contract.rs uses — where the whole string is compared, because the counts are what move).
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
# have opened /data instead (ADR-0009).
case_schema_version() {
  local name=case_schema_version banner database
  database="$WORK/data/app.db"
  banner="$(grep -m1 '^database ' "$serve_err" || true)"
  check "$name/banner" "database $database schema $schema_version" "$banner"
  [ -f "$database" ] ||
    fail "$name/app_db" "serve did not write $database: $(ls -A "$WORK/data" 2>/dev/null)"
  assertions=$((assertions + 1))
}

# the acceptance is "migration adds `sessions`", and nothing else proves the booted
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

# the acceptance is the `events` table and the `last_seq` counter it bumps. Same byte grep as
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

# Box 1: the `Host` header is not one this machine answers. Only the host is wrong.
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

# a create answers 202 with the row, a `Location` and the event stream's URL. The id is
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

# the same key answers the same session with a second 202, not a second row.
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

# a project that is not one path segment is refused, and the code says so.
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

# one live session against the boot config's cap of one, so a fresh create is refused
# with 409 `cap_reached` (nothing is evictable until).
# those stories: a create at the cap is refused when there is nothing to evict, and it is
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
  # The sentence says why nothing was evicted, which is the create's own answer.
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

# the `events` URL a create answers with is a server-sent event stream. No producer exists
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

# the backend is a machine setting, so a config file saying `backend = "stub"` is what
# this boot runs on, with no flag. The banner is what proves it took effect; the provisioning case
# below is what proves the stub is the backend that actually provisioned the session.
case_backend_from_config() {
  local name=case_backend_from_config banner
  banner="$(sed -n 's/^holosmith .* backend \([a-z]*\)$/\1/p' "$serve_err" | sed -n '1p')"
  check "$name/banner" "stub" "$banner"
}

# a create hands off to the provisioner, which reports the four steps and ends the session
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

# the state machine's `session.status` payload. The four step reports claim no state
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

# the sessions list. A `GET`, so only the host check applies, and the body carries the
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
  # The row's *state* is deliberately not pinned: the provisioner starts runs while these
  # cases run, so `starting` or `idle` are both correct by the time this answer is read.
}

# one session by id, and the screen path that opens with it. The one row is the list's
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

# one tool call's full output is a file, and the route serves it — while a name that cannot be a
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

# an image in a tool result is a file beside the output, and the route serves its bytes with the
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

# the other half, on a real daemon: the names a crash leaves behind that no row owns. The live session's
# container is replaced under the same name (which is what "no name conflict" means), a container and a network
# with no row go, and a worktree directory no row owns goes **while its branch and its snapshot ref stay**
# (ADR-0052). The machine's own `hs_egress`/`hs_key` are not the build's session names and are left.
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
  # **What a crash must never cost**: the branch and the snapshot ref.
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


# this machine's isolation, measured. The probe is `tests/selftest.rs`'s subject too, with a bridge
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

  # the pair, in the boot's own words: the probe sees the proxy once it is joined — which is what makes the off
  # case's silence evidence — or the machine had no proxy to join, which the boot also says. A run that says
  # neither is a pair that was measured by nobody, and a `debug` line is why this case boots with `HOLOSMITH_LOG=debug`.
  grep -q 'the probe sees the egress proxy once it is joined' "$serve_err" ||
    grep -q 'the egress proxy is not running on this machine' "$serve_err" ||
    fail "$name/pair" "the boot neither saw the joined proxy nor said it had none: $(grep -i 'egress' "$serve_err" | tr '\n' ' ')"
  assertions=$((assertions + 1))

  # the inverse pair: the key proxy must answer on the probe's network. A boot that did not start it says
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

# the WIP snapshot as the shipped artifact runs it. No daemon, no server: a real repository and a
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

# the key store and the key proxy's refusal path, with no daemon and no internet. The forwarding half
# to a provider is the (a gate that needs a real key is not a gate); what this case can prove is the part
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

#  (V3's bytes): the request the key proxy hands its TLS client — head **and** body. The TLS client is a
# process (`openssl`), so a shim on `PATH` stands in for it: it takes one bounded read of exactly what the proxy
# writes, saves it, and answers a canned 200. What the case can then assert is the thing that was wrong: the
# request carries the body, names its length once, and has the stored key swapped in. No internet, no real key
# and no daemon — a gate cannot have those, and the real forward is the recorded demo.
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

# the egress proxy, on the wire and with no daemon and no internet. What it proves is the four
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

#  (V6): the Internet-access checkbox is **topology**, not configuration. Two real sessions, one per
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
    # has been bitten by twice.
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
    # `case_key_proxy`'s and the provider path is that story's.
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
          # story's: a machine whose proxy container is **not running** (a supported machine)
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
  # keep reading the store as it was at mount time and answer `bad-key` for every session.
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

# a database from a **newer** release. `serve` continues — item 10's rollback must not need a restore, and
# expand/contract is what makes continuing safe — and it says so in two places an operator reads: the banner's
# `schema <n>` is the *recorded* number rather than the binary's, and one `warn` names both versions and the command
# that refuses (`doctor`). This is the observable half of the acceptance's third bullet; the pure verdict
# and its sentence are `tests/schema_previous.rs`'s.
#  (spec item 3, V7): the preview listener. Two halves.
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
  # database rather than at the machine's `/data`.
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
  # …and the command keeps the promise the warning made: `doctor --quick` refuses the same database,
  # read **without** migrating it, with `schema::verdict`'s one sentence. `serve` continues (the 200 below),
  # `doctor` does not — that split is ADR-0047's whole decision.
  status=0
  "$artifact" doctor --quick --config "$WORK/config-schema-newer.toml" --root "$root" \
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

# the session screen is a path, not a route. `/sessions/<id>` is not a file the app ships,
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

# one prompt, one turn. The 202 is the row, now `running`, and the stream read until the
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
  # coalescing has nothing to merge; a slower window would make this a shorter list.
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

# the proof from the artifact: a turn's log holds one delta row per run of deltas the writer
# committed together — never more rows than the script emitted — and every word the script said is still
# there, in order, under one message id.
#
# What this case cannot show is the merge itself. The stub paces one frame per 200 ms, eight commit windows
# apart, so nothing here is coalesced: the merge is `tests/turns.rs`'s (seven scripted deltas -> three rows
# at a pace below the window) and `tests/events.rs`'s (5,000 -> three). What the shipped artifact can prove
# is the contract a merge must keep, which is what a burst producer is held to.
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

# the turn timeout does not fire on a turn that ends. The gate cannot make it *fire* —
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

# a prompt's body is one non-empty `prompt`, and a body that is not is 422 `bad_body` — the
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

# a prompt for a session the table does not hold is ADR-0008's 404 `not_found`, decided
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

# abort answers 202 with the running row, the ladder ends the turn off-request, and the
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

# an abort with no turn to abort is `409 session_idle` — the code the family gained for it —
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

# a steer hands guidance to the running turn, answers 202 with the row, and is recorded as a
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

# a steer with no running turn is `409 session_idle`, the code added for it.
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

# wait_for_idle_id <label> <id>: the same for any session, which the eviction case needs because
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

# a resume with nothing to bring back is `409 session_idle`. The happy path cannot be driven
# from this harness: nothing in the API produces `stopped` yet — the policies that do belong to later changes — so `tests/resume.rs` proves the round trip against a row the table stopped.
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

# a resume for a session the table does not hold is ADR-0008's 404 `not_found`.
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
# The removal is a background task's, so the 202 is not the proof that it happened: the row
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
# once. The idle stop is a timer's, not a request's, so its budget is the config's bound plus
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
# `force` is the `?force=true`, and a Docker run needs it: the WIP snapshot the delete asks the backend
# for before it removes anything is that story's, so a Docker session with a worktree answers `409 unsaved_work`
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

# the delete flow's happy path, against the stub backend whose snapshot and delete both
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

# the delete's own answer, and the page that shows it. Two facts are worth asserting on every
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

# the stopped-session expiry. A session stopped longer than `stopped_ttl_days` is removed by the
# artifact's own daily job, through the delete flow — so this case measures the *removal* and, because a
# delete's whole promise is what it keeps, the branch and the WIP ref that survive it.
#
# It boots its own server over a real repository and the daemon, for `case_disk_quota`'s reason: the worktree
# and the branch have to be real for "using the delete flow" to mean anything, and a stub session has neither.
# The staleness is written, not waited for — `stopped_at` into the past with `python3`'s `sqlite3` — because a
# seven-day wall clock is not something a gate can run, and the column *is* the clock. That is also how the
# second half is possible: an `error` row with a stale `stopped_at` is a row no writer of this build can make,
# so only the candidate query's `status = 'stopped'` predicate can keep it — the isolation `tests/expiry.rs`
# cannot construct.
case_stopped_expiry() {
  local name=case_stopped_expiry
  local image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local root="$repo_root/target/verify-expiry-$$"
  local repo="$root/projects/holosmith"
  local data="$root/data"
  local database="$root/data/app.db"
  local response body id other head branch ref wt stale needle
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/expiry.rs covers the policy, tests/worktree.rs the keep-rules)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to backdate the row (tests/expiry.rs covers the policy)\n' "$name"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$repo" || fail "$name/mkdir" "could not create $repo"
  git -C "$repo" init --quiet -b main . || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work"
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "docker"\nprojects_root = "%s"\nagent_image = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$root/projects" "$image" >"$WORK/config-expiry.toml"
  start_serve "$WORK/config-expiry.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-expiry-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  docker_session_id="$id"
  wt="$root/projects/.worktrees/$id"
  branch="agent/$id"
  ref="refs/holosmith/wip/$id"
  wait_for_idle_docker "${name}_create" "$id"

  # The agent's work, simulated — this case is about what the *expiry* keeps, not about what an agent writes:
  # one commit on the branch (so the branch is ahead of its base and in no target, and is therefore kept) and
  # one change the flow's own snapshot is what saves (so the WIP ref exists too).
  printf 'committed\n' >"$wt/work.txt" || fail "${name}/commit" "could not write in the worktree"
  git -C "$wt" -c user.name=verify -c user.email=verify@example add work.txt ||
    fail "${name}/commit" "git add did not work in the worktree"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet \
    -m "the agent's commit" || fail "${name}/commit" "git commit did not work in the worktree"
  head="$(git -C "$wt" rev-parse HEAD)"
  printf 'uncommitted\n' >"$wt/notes.txt"

  # A second session, which the case then writes into a shape no writer of this build produces: `error` with a
  # stale `stopped_at`. Item 15 says an `error` session is never auto-deleted, and the candidate query's status
  # predicate is the only thing standing between this row and the job.
  fetch_body "${name}_other" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-expiry-2" ||
    fail "${name}_other/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_other" "$(answer "${name}_other")" 202
  body="$(cat "$WORK/responses/${name}_other.body")"
  other="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$other" ] || fail "${name}_other/id" "the response names no id: $body"
  wait_for_idle_docker "${name}_other" "$other"

  # Eight days ago on the default seven-day window: past it. The restart is the story's own demo — the job's
  # first pass is immediate at boot, which is what makes a daily policy observable in a gate.
  stale=$(( $(date +%s) - 8 * 86400 ))
  python3 - "$database" "$id" "$other" "$stale" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
connection.execute(
    "UPDATE sessions SET status = 'stopped', status_reason = NULL, stopped_at = ? WHERE id = ?",
    (int(sys.argv[4]), sys.argv[2]))
# A row the build cannot write: `error` with a stop clock. Only the candidate query's status predicate can
# keep this out of the job's hands.
connection.execute(
    "UPDATE sessions SET status = 'error', status_reason = 'provisioning_failed', stopped_at = ? WHERE id = ?",
    (int(sys.argv[4]), sys.argv[3]))
connection.commit()
connection.close()
PY
  stop_serve
  start_serve "$WORK/config-expiry.toml" "$data"

  # 1. The removal, and that it is the *delete flow's*: the row is gone from the API and from the table.
  wait_for_gone "${name}_gone" "$id"
  fetch "${name}_list" GET /api/sessions "Host: 127.0.0.1" ||
    fail "${name}_list/no_answer" "GET /api/sessions did not answer"
  body="$(cat "$WORK/responses/${name}_list.body")"
  case "$body" in
    *"\"id\":\"$id\"") fail "${name}/listed" "the expired session is still on the list: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  # The machine's window rides the list's answer (item 15's "days remaining" needs the number, not the row).
  printf '%s\n' "$body" | grep -qF '"stopped_ttl_days":7' ||
    fail "${name}/ttl" "the list does not carry the machine's retention window: $body"
  assertions=$((assertions + 1))

  # 2. What the delete flow kept: the branch at the commit the agent made, and the WIP ref the snapshot wrote.
  #    This is the "using the delete flow from item 9" claim, measured rather than asserted.
  check "${name}/branch" "$head" "$(git -C "$repo" rev-parse --verify --quiet "refs/heads/$branch")"
  git -C "$repo" rev-parse --verify --quiet "$ref" >"$WORK/responses/$name.ref" ||
    fail "${name}/ref" "the snapshot ref did not survive the expiry"
  assertions=$((assertions + 1))

  # 3. What it left behind: nothing. The worktree, the session's directory, the container and the network are all
  #    gone. Which of them the delete flow took and which a `--rm` run took when the server it was attached to was
  #    stopped is deliberately not asserted — this is the acceptance's claim ("its container and network gone"),
  #    and it is an outcome. The network is the one only `backend.delete` removes, so the flow's teardown is
  #    measured there.
  [ ! -e "$wt" ] || fail "${name}/worktree" "the worktree is still there: $wt"
  assertions=$((assertions + 1))
  [ ! -e "$data/sessions/$id" ] || fail "${name}/session_dir" "the session directory is still there"
  assertions=$((assertions + 1))
  if docker inspect "s_$id" >/dev/null 2>&1; then
    fail "${name}/container" "the session's container is still there: s_$id"
  fi
  assertions=$((assertions + 1))
  if docker network inspect "agents_$id" >/dev/null 2>&1; then
    fail "${name}/network" "the session's network is still there: agents_$id"
  fi
  assertions=$((assertions + 1))

  # ...and the job's own line, which is the only trace a removal leaves once the row is gone (`case_disk_quota`
  # asserts its policy's line the same way). A daily policy that fails silently is the failure mode the story's
  # "skip and retry" wording is about, so the line is part of the contract.
  grep -q 'holosmith::expiry' "$serve_err" ||
    fail "${name}/log" "the job logged nothing about the pass: $(tail -5 "$serve_err")"
  assertions=$((assertions + 1))

  # 4. An `error` row is never auto-deleted, however stale its clock.
  fetch "${name}_error" GET "/api/sessions/$other" "Host: 127.0.0.1" ||
    fail "${name}_error/no_answer" "GET /api/sessions/$other did not answer"
  response="$(answer "${name}_error")"
  expect_status "${name}_error" "$response" 200
  body="$(cat "$WORK/responses/${name}_error.body")"
  printf '%s\n' "$body" | grep -qF '"status":"error"' ||
    fail "${name}/error_row" "the error row was not left as it was: $body"
  assertions=$((assertions + 1))

  # 5. The sentence the Sessions screen draws (item 15's last bullet). No gate may need a browser, so the page's
  #    copy is grepped out of the bundle it ships, and the arithmetic is `expiryText`'s, which `npm test` owns.
  for needle in "expires in " "expires within a day" "past its retention window"; do
    grep -qF -- "$needle" web/dist/assets/*.js ||
      fail "${name}/page" "the shipped page does not carry \"$needle\" in web/dist/assets/*.js"
    assertions=$((assertions + 1))
  done

  # The machine as it was found: this case's sessions, container, network and scratch root go, and the shared
  # boot comes back (`case_disk_quota`'s idiom — a case that stopped it and left `serve_pid` empty fails the
  # *next* `stop_serve`).
  remove_session "${name}_cleanup" "$other" force
  docker rm --force "s_$other" >/dev/null 2>&1 || true
  docker network rm "agents_$other" >/dev/null 2>&1 || true
  docker rm --force "s_$id" >/dev/null 2>&1 || true
  docker network rm "agents_$id" >/dev/null 2>&1 || true
  docker_session_id=""
  rm -rf "$root"
  stop_serve
  start_serve
}

# the Web Push notification (spec item 11). A **daemon-free** case: the stub backend, a real TLS
# receiver written in `node`, and the artifact's own sender talking to it over TLS.
#
# What it measures, and why each half needs the ingredient it uses:
#
# - **The request a push service receives.** A `node:https` listener on `localhost` prints its port, answers
#   `201`/`410`/`403` by path and appends one JSON line per request. It verifies the VAPID token's ES256
#   signature **against the pair this case generated** (`crypto.verify` with a `P-256` JWK), decrypts the body
#   per RFC 8291 with the subscription's own private key, and reports whether the `aud` claim is its own origin.
#   That makes "signed with the machine's pair and encrypted to the subscription" a measurement rather than a
#   claim: nothing here trusts the sender's word.
# - **The store, through the API.** A subscribe, a re-subscribe of the same endpoint (one row, new keys, the
#   original `created_at`), the `422` refusals, and a `DELETE` that removes the row and then answers `404`.
# - **The stream-open rule** (item 11's "unless an SSE stream for that session is open") and **one trigger the
#   stub can reach from HTTP**: the token budget. A session whose `max_session_tokens` is one spends its budget
#   on the stub turn's own `message.end`, so a prompt ends with reason `budget_exceeded` — which is item 11's
#   fourth named trigger. The stream pair is measured by prompting twice: once with the stream held (no line
#   arrives) and once without it (exactly one line, with `kind`, `session_id`, `host` and both checks true).
# - **The disposition table at the far end.** The `/gone/` endpoint answers `410` and its row disappears; the
#   `/keyed/` endpoint answers `403` and its row stays, because a `403` is a *key* problem rather than a dead
#   endpoint (ADR-0071's D9).
#
# What it does **not** measure, and why: the long-turn and `error` triggers. The stub's turn is a fixed script
# with no failure mode reachable over HTTP, and `long_turn` is twenty seconds of spec with no config key to
# shorten it — those two are `tests/push.rs`'s (`three_triggers_and_the_negative_reach_the_recorder`). A real
# push service and a real browser subscription are nobody's test environment; the receiver stands in for both,
# and `docs/build.md` says so.
#
# `SSL_CERT_FILE` is exported around the boot and unset immediately after: `openssl s_client` honours it for its
# default trust store, which is what lets a private CA be configured the way every other tool is configured —
# the reason the sender passes no `-CAfile` (ADR-0071's D2). An exported override must not leak into another
# case's boot.
# The push receiver: the *browser* in a push exchange, written once for the two cases that need one.
#
# the `case_push` reads one machine's payload: the token, the record, the disposition for each answer a
# push service can give. the `case_cross_machine` reads the same three fields from a **peer's** payload
# through a hub, so it needs the same arithmetic with different keys — and a second copy of this file would be
# a second thing to keep right (this script already found its own two bugs: a JWK without `format: 'jwk'` and
# an auth tag set after `final()`).
write_push_receiver() {
  cat >"$WORK/push-receiver.mjs" <<'JS'
import crypto from 'node:crypto';
import fs from 'node:fs';
import https from 'node:https';

// The *browser* in this exchange: it verifies the token against the case's own application-server key and
// decrypts the record with its own subscription keys, using nothing but node's crypto. A receiver written with
// the sender's arithmetic would agree with any mistake the sender made, which is why this half is separate code
// in another language.
const [certFile, keyFile, logFile, vapidPublicB64, uaPrivateHex, uaPublicHex, authHex] = process.argv.slice(2);
const b64 = (text) => Buffer.from(String(text).replace(/=+/g, ''), 'base64url');
const vapidPublic = b64(vapidPublicB64);
const uaPrivate = Buffer.from(uaPrivateHex, 'hex');
const uaPublic = Buffer.from(uaPublicHex, 'hex');
const auth = Buffer.from(authHex, 'hex');
// A KeyObject, not a bare JWK: `crypto.verify` takes a JWK object only with `format: 'jwk'`, and a receiver that
// passed it bare would report "the machine's signature does not verify" for a signature that does (measured --
// this is what `case_push`'s first run said).
const vapidKey = crypto.createPublicKey({
  key: {
    kty: 'EC',
    crv: 'P-256',
    x: vapidPublic.subarray(1, 33).toString('base64url'),
    y: vapidPublic.subarray(33, 65).toString('base64url'),
  },
  format: 'jwk',
});
const hmac = (key, ...parts) => {
  const h = crypto.createHmac('sha256', key);
  for (const part of parts) h.update(part);
  return h.digest();
};

// RFC 8291 §3.4 from the receiver's side: one ECDH, three HMAC chains and one AES-128-GCM record.
function decrypt(body) {
  try {
    const salt = body.subarray(0, 16);
    const idlen = body[20];
    const asPublic = body.subarray(21, 21 + idlen);
    if (asPublic.length !== 65) return null;
    const tag = body.subarray(body.length - 16);
    const ciphertext = body.subarray(21 + idlen, body.length - 16);
    const ecdh = crypto.createECDH('prime256v1');
    ecdh.setPrivateKey(uaPrivate);
    const secret = ecdh.computeSecret(asPublic);
    const prkKey = hmac(auth, secret);
    const ikm = hmac(prkKey, Buffer.from('WebPush: info\0'), uaPublic, asPublic, Buffer.from([1]));
    const prk = hmac(salt, ikm);
    const hk = (info, n) =>
      hmac(prk, Buffer.from(info + '\0'), Buffer.from([1])).subarray(0, n);
    const decipher = crypto.createDecipheriv(
      'aes-128-gcm',
      hk('Content-Encoding: aes128gcm', 16),
      hk('Content-Encoding: nonce', 12),
    );
    decipher.setAuthTag(tag);
    const record = Buffer.concat([decipher.update(ciphertext), decipher.final()]);
    // RFC 8188's padding: zero octets, then the delimiter -- `0x02` on the last record.
    let end = record.length;
    while (end > 0 && record[end - 1] === 0) end -= 1;
    if (end === 0 || record[end - 1] !== 0x02) return null;
    return JSON.parse(record.subarray(0, end - 1).toString('utf8'));
  } catch {
    return null;
  }
}

const server = https.createServer(
  { cert: fs.readFileSync(certFile), key: fs.readFileSync(keyFile) },
  (request, response) => {
    const chunks = [];
    request.on('data', (chunk) => chunks.push(chunk));
    request.on('end', () => {
      const body = Buffer.concat(chunks);
      const authorization = request.headers.authorization ?? '';
      // `Authorization: vapid t=<jwt>, k=<key>`
      const match = /^vapid\s+t=([^,\s]+)\s*,\s*k=(\S+)$/.exec(authorization);
      let jwtOk = false;
      let audOk = false;
      if (match) {
        const parts = match[1].split('.');
        if (parts.length === 3) {
          try {
            jwtOk = crypto.verify(
              'sha256',
              Buffer.from(`${parts[0]}.${parts[1]}`),
              { key: vapidKey, dsaEncoding: 'ieee-p1363' },
              b64(parts[2]),
            );
            const claims = JSON.parse(b64(parts[1]).toString('utf8'));
            audOk = claims.aud === `https://localhost:${server.address().port}`;
          } catch {
            jwtOk = false;
          }
        }
      }
      const line = {
        path: request.url,
        authorized: Boolean(match),
        ttl: request.headers.ttl ?? null,
        encoding: request.headers['content-encoding'] ?? null,
        jwt_ok: jwtOk,
        aud_ok: audOk,
        payload: decrypt(body),
      };
      fs.appendFileSync(logFile, `${JSON.stringify(line)}\n`);
      const answer = request.url.startsWith('/gone/') ? 410 : request.url.startsWith('/keyed/') ? 403 : 201;
      response.writeHead(answer, { 'Content-Length': 0 });
      response.end();
    });
  },
);
server.listen(0, '127.0.0.1', () => {
  console.log(`PORT ${server.address().port}`);
});
JS
}

# The SEC1 walk: a 121-octet `openssl ecparam` key read as the two base64url strings a `vapid.toml` holds and
# the two hex strings a receiver needs. `python3` reads the DER rather than a second hand parser in bash.
#
# One writer, two cases: the peer's pair, the
# subscription's pair and a hub's own pair are the same three questions about the same 121 octets.
write_sec1_walk() {
  cat >"$WORK/sec1.py" <<'PY'
import base64, sys
der = open(sys.argv[1], "rb").read()
# One TLV: the tag, the content's first offset and the offset after it. Every length in a 121-octet SEC1 key is
# short-form (the longest is the 0x77 SEQUENCE), so one length octet is the whole of the rule -- and a long form is
# refused rather than misread, which is the same rule `vapid::from_der` keeps in Rust.
def element(at):
    tag = der[at]
    length = der[at + 1]
    assert length < 0x80, "a long-form length in a 121-octet key"
    start = at + 2
    return tag, start, start + length
tag, body, end = element(0)
assert tag == 0x30, "not a SEQUENCE: %r" % der[:4]
tag, _, after_version = element(body)
assert tag == 0x02, "no version: %r" % der[:4]
tag, start, after_scalar = element(after_version)
assert tag == 0x04, "no private scalar: %r" % der[:4]
scalar = der[start:after_scalar]
tag, _, after_curve = element(after_scalar)
assert tag == 0xa0, "no named curve"
tag, start, after_public = element(after_curve)
assert tag == 0xa1, "no public key"
tag, start, after_bits = element(start)
assert tag == 0x03 and der[start] == 0x00, "no BIT STRING"
point = der[start + 1:after_bits]
assert len(point) == 65 and point[0] == 0x04, "not an uncompressed point"
assert after_bits == end, "trailing octets after the public key"
b64 = lambda value: base64.urlsafe_b64encode(value).rstrip(b"=").decode()
print(b64(point), b64(scalar), scalar.hex(), point.hex())
PY
}

case_push() {
  local name=case_push
  local root="$repo_root/target/verify-push-$$"
  local repo="$root/projects/holosmith"
  local data="$root/data"
  local tls="$root/tls"
  local response body id fresh created_at rows port_line push_port=""
  local before sent
  local vapid_public vapid_private subscription_public subscription_private_hex
  local subscription_public_hex auth_b64 auth_hex rotated rotated_hex
  local ok_endpoint gone_endpoint keyed_endpoint
  local line
  if ! command -v openssl >/dev/null 2>&1; then
    printf 'SKIP %s: no openssl to sign a token or serve TLS (tests/push.rs covers the JWT and RFC 8291)\n' "$name"
    return 0
  fi
  if ! command -v node >/dev/null 2>&1; then
    printf 'SKIP %s: no node to run the TLS receiver (tests/push.rs covers the sender)\n' "$name"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to read the SEC1 keys and the database (tests/push.rs covers the rows)\n' "$name"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$repo" "$tls" "$data/secrets" ||
    fail "$name/mkdir" "could not create $root"
  chmod 0700 "$data/secrets"
  git -C "$repo" init --quiet -b main . || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work"

  # The SEC1 walk, in one place: `openssl ecparam … -outform DER` writes 121 octets and the two strings a
  # `vapid.toml` holds are its scenic halves. `python3` reads the DER rather than a second hand parser in bash.
  write_sec1_walk

  # The machine's own pair, generated the way `server deploy` generates it and written the way `vapid::render`
  # writes it — header line included, because a file an operator may open should say what it is.
  openssl ecparam -name prime256v1 -genkey -noout -outform DER -out "$tls/vapid.der" ||
    fail "$name/ecparam" "openssl ecparam did not write a key"
  read -r vapid_public vapid_private _ _ < <(python3 "$WORK/sec1.py" "$tls/vapid.der")
  [ -n "$vapid_public" ] && [ -n "$vapid_private" ] ||
    fail "$name/vapid" "the generated pair did not parse"
  printf '# The machine'"'"'s Web Push key pair. `server deploy` writes this once and never rewrites it.\npublic_key = "%s"\nprivate_key = "%s"\n' \
    "$vapid_public" "$vapid_private" >"$data/secrets/vapid.toml" ||
    fail "$name/vapid" "could not write $data/secrets/vapid.toml"
  chmod 0600 "$data/secrets/vapid.toml"

  # The subscription: its own pair, and sixteen octets of `auth`. Both go to the receiver, because the
  # receiver is the *browser* in this exchange and only it can read what the machine sealed.
  openssl ecparam -name prime256v1 -genkey -noout -outform DER -out "$tls/subscription.der" ||
    fail "$name/ecparam" "openssl ecparam did not write the subscription's key"
  read -r subscription_public _ subscription_private_hex subscription_public_hex \
    < <(python3 "$WORK/sec1.py" "$tls/subscription.der")
  auth_hex="$(openssl rand -hex 16)"
  auth_b64="$(printf '%s' "$auth_hex" | python3 -c \
    'import base64, sys; print(base64.urlsafe_b64encode(bytes.fromhex(sys.stdin.read())).rstrip(b"=").decode())')"
  [ -n "$auth_b64" ] || fail "$name/auth" "openssl rand did not answer 16 octets"
  # The secret the *re-subscribe* arm rotates to. The receiver holds this one, because it plays the browser that
  # re-subscribed: `/ok/`'s body therefore only decrypts if the machine really stored the new secret.
  rotated_hex="$(openssl rand -hex 16)"
  rotated="$(printf '%s' "$rotated_hex" | python3 -c \
    'import base64, sys; print(base64.urlsafe_b64encode(bytes.fromhex(sys.stdin.read())).rstrip(b"=").decode())')"

  # The TLS material: a CA and a `localhost` certificate with a `DNS:` SAN, because `-verify_hostname` matches
  # the name in the certificate and an IP SAN would not do.
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$tls/ca.key" -out "$tls/ca.pem" -days 1 \
    -subj /CN=VerifyPushCA >/dev/null 2>&1 ||
    fail "$name/ca" "openssl req did not write a CA"
  openssl req -newkey rsa:2048 -nodes -keyout "$tls/server.key" -out "$tls/server.csr" \
    -subj /CN=localhost >/dev/null 2>&1 ||
    fail "$name/cert" "openssl req did not write a CSR"
  printf 'subjectAltName=DNS:localhost\n' >"$tls/server.ext"
  openssl x509 -req -in "$tls/server.csr" -CA "$tls/ca.pem" -CAkey "$tls/ca.key" \
    -CAcreateserial -out "$tls/server.pem" -days 1 -extfile "$tls/server.ext" >/dev/null 2>&1 ||
    fail "$name/cert" "openssl x509 did not sign the server certificate"

  # The receiver. It is the browser: it verifies the token against the **case's** VAPID public key, decrypts the
  # record with its own private key, and appends one JSON line per request so the assertions below read what
  # arrived rather than what the sender said it sent.
  write_push_receiver

  rm -f "$WORK/push-received.jsonl"
  : >"$WORK/push-received.jsonl"
  node "$WORK/push-receiver.mjs" "$tls/server.pem" "$tls/server.key" "$WORK/push-received.jsonl" \
    "$vapid_public" "$subscription_private_hex" "$subscription_public_hex" "$rotated_hex" \
    >"$WORK/push-receiver.out" 2>"$WORK/push-receiver.err" &
  local receiver_pid=$!
  # Registered with the trap as well as held here: a `fail` below exits the script, and this is the only other
  # thing that would leave a `node` listener behind on a port the next run could bind.
  extra_pid=$receiver_pid
  for _ in $(seq 1 100); do
    port_line="$(sed -n 's/^PORT \([0-9]*\)$/\1/p' "$WORK/push-receiver.out" | sed -n '1p')"
    [ -n "$port_line" ] && break
    kill -0 "$receiver_pid" 2>/dev/null || break
    sleep 0.05
  done
  if [ -z "$port_line" ]; then
    kill "$receiver_pid" 2>/dev/null || true
    fail "$name/receiver" "the push receiver did not bind: $(cat "$WORK/push-receiver.err")"
  fi
  push_port="$port_line"
  ok_endpoint="https://localhost:$push_port/ok/1"
  gone_endpoint="https://localhost:$push_port/gone/1"
  keyed_endpoint="https://localhost:$push_port/keyed/1"

  # The boot: the stub backend, a budget of one token so the stub turn's own `message.end` trips it, no idle
  # stop, and this case's own roots. `SSL_CERT_FILE` is exported for exactly this `start_serve` and unset at
  # once.
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "stub"\nmax_session_tokens = 1\nidle_stop_min = 0\nprojects_root = "%s"\nmin_free_gb = 1\n' \
    "$owner" "$root/projects" >"$WORK/config-push.toml"
  export SSL_CERT_FILE="$tls/ca.pem"
  start_serve "$WORK/config-push.toml" "$data"
  unset SSL_CERT_FILE

  # 1. The store, through the API. One subscribe is one row.
  fetch_body "${name}_sub" POST /api/push/subscribe \
    "{\"endpoint\":\"$ok_endpoint\",\"p256dh\":\"$subscription_public\",\"auth\":\"$auth_b64\"}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_sub/no_answer" "POST /api/push/subscribe did not answer"
  expect_status "${name}_sub" "$(answer "${name}_sub")" 204
  rows="$(python3 - "$data/app.db" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
for row in connection.execute("SELECT endpoint, p256dh, auth, created_at FROM push_subscriptions ORDER BY endpoint"):
    print("|".join(str(value) for value in row))
connection.close()
PY
)"
  [ "$(printf '%s\n' "$rows" | grep -c .)" = 1 ] ||
    fail "${name}/row" "one subscribe is not one row: $rows"
  assertions=$((assertions + 1))
  created_at="$(printf '%s\n' "$rows" | sed -n 's/^.*|\([0-9][0-9]*\)$/\1/p')"

  # A re-subscribe of the same endpoint with a rotated `auth`: still one row, the new secret, the original
  # `created_at` — a browser re-subscribing does not make itself a second device.
  fetch_body "${name}_resub" POST /api/push/subscribe \
    "{\"endpoint\":\"$ok_endpoint\",\"p256dh\":\"$subscription_public\",\"auth\":\"$rotated\"}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_resub/no_answer" "the re-subscribe did not answer"
  expect_status "${name}_resub" "$(answer "${name}_resub")" 204
  check "${name}/resub_row" "$ok_endpoint|$subscription_public|$rotated|$created_at" \
    "$(python3 - "$data/app.db" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
rows = list(connection.execute("SELECT endpoint, p256dh, auth, created_at FROM push_subscriptions"))
connection.close()
print("|".join(str(value) for value in rows[0]) if len(rows) == 1 else ", ".join(str(row) for row in rows))
PY
)"

  # The other two subscriptions the disposition table needs, plus the `422` arms.
  fetch_body "${name}_gone" POST /api/push/subscribe \
    "{\"endpoint\":\"$gone_endpoint\",\"p256dh\":\"$subscription_public\",\"auth\":\"$rotated\"}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_gone/no_answer" "the second subscribe did not answer"
  expect_status "${name}_gone" "$(answer "${name}_gone")" 204
  fetch_body "${name}_keyed" POST /api/push/subscribe \
    "{\"endpoint\":\"$keyed_endpoint\",\"p256dh\":\"$subscription_public\",\"auth\":\"$rotated\"}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_keyed/no_answer" "the third subscribe did not answer"
  expect_status "${name}_keyed" "$(answer "${name}_keyed")" 204

  local short_point short_auth
  short_point="$(python3 -c 'import base64; print(base64.urlsafe_b64encode(b"\x04"*64).rstrip(b"=").decode())')"
  short_auth="$(python3 -c 'import base64; print(base64.urlsafe_b64encode(b"\x00"*15).rstrip(b"=").decode())')"
  local -a refusals=(
    "http://localhost:$push_port/ok/1|$subscription_public|$rotated|bad_endpoint"
    "https://|$subscription_public|$rotated|bad_endpoint"
    "$ok_endpoint|$short_point|$rotated|bad_subscription_key"
    "$ok_endpoint|$subscription_public|$short_auth|bad_subscription_key"
  )
  local arm arm_endpoint arm_point arm_auth arm_code
  for arm in "${refusals[@]}"; do
    IFS='|' read -r arm_endpoint arm_point arm_auth arm_code <<<"$arm"
    fetch_body "${name}_bad" POST /api/push/subscribe \
      "{\"endpoint\":\"$arm_endpoint\",\"p256dh\":\"$arm_point\",\"auth\":\"$arm_auth\"}" \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "${name}_bad/no_answer" "a refused subscribe did not answer"
    expect_status "${name}_bad" "$(answer "${name}_bad")" 422
    body="$(cat "$WORK/responses/${name}_bad.body")"
    case "$body" in
      *"\"code\":\"$arm_code\""*) assertions=$((assertions + 1)) ;;
      *) fail "${name}/refusal" "the refusal is not $arm_code: $body" ;;
    esac
  done
  # A fourth key, and a `DELETE` that names nothing.
  fetch_body "${name}_extra" POST /api/push/subscribe \
    "{\"endpoint\":\"$ok_endpoint\",\"p256dh\":\"$subscription_public\",\"auth\":\"$rotated\",\"keys\":{}}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_extra/no_answer" "a subscribe with a fourth key did not answer"
  body="$(cat "$WORK/responses/${name}_extra.body")"
  case "$body" in
    *'"code":"unknown_field"'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}/extra" "a fourth body key is not unknown_field: $body" ;;
  esac
  fetch "${name}_noendpoint" DELETE /api/push/subscribe \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_noendpoint/no_answer" "DELETE with no endpoint did not answer"
  expect_status "${name}_noendpoint" "$(answer "${name}_noendpoint")" 422
  body="$(cat "$WORK/responses/${name}_noendpoint.body")"
  case "$body" in
    *'"code":"bad_query"'*) assertions=$((assertions + 1)) ;;
    *) fail "${name}/noendpoint" "a DELETE with no endpoint is not bad_query: $body" ;;
  esac

  # 2. The stream pair and the one trigger the stub can reach. A session, prompted twice: once with its stream
  #    held (item 11's "unless an SSE stream for that session is open") and once without.
  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-push-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  wait_for_idle_id "${name}_create" "$id"

  # The stream is **held open across the turn**, which is the whole of item 11's "unless an SSE stream for that
  # session is open": `fetch_stream` closes its socket after the header block, so the guard would be released
  # before the prompt and this arm would measure nothing. The file descriptor therefore stays open here: the
  # header block is read (which is what proves the handler registered), the prompt runs, the budget fires, and
  # only then is the socket closed. The frames that arrive meanwhile are not read, which is what a locked phone
  # looks like from this side.
  local stream_fd stream_line
  exec {stream_fd}<> "/dev/tcp/127.0.0.1/$port" || fail "${name}_stream/open" "no connection to the events route"
  printf 'GET /api/sessions/%s/events HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: %s\r\nX-Holosmith-Client: 1\r\nTailscale-User-Login: %s\r\nConnection: close\r\n\r\n' \
    "$id" "$control_origin" "$owner" >&"$stream_fd" ||
    fail "${name}_stream/request" "the events request was not written"
  : >"$WORK/responses/${name}_stream.http"
  while IFS= read -r -t 5 stream_line <&"$stream_fd"; do
    printf '%s\n' "$stream_line" >>"$WORK/responses/${name}_stream.http"
    [ "$stream_line" != $'\r' ] || break
  done
  sed '1,/^\r$/d' "$WORK/responses/${name}_stream.http" >"$WORK/responses/${name}_stream.body"
  expect_status "${name}_stream" "$(answer "${name}_stream")" 200
  fetch_body "${name}_prompt" POST "/api/sessions/$id/prompt" '{"prompt":"spend it"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt/no_answer" "POST a prompt did not answer"
  expect_status "${name}_prompt" "$(answer "${name}_prompt")" 202
  wait_for_reason_id "${name}_held_row" "$id" budget_exceeded
  # A moment's grace so a send that raced the guard would land in the receiver's log before it is read.
  sleep 1
  exec {stream_fd}<&-
  # The stream was open for that turn, so nothing may have been sent for this session.
  if grep -qF "\"session_id\":\"$id\"" "$WORK/push-received.jsonl"; then
    fail "${name}/stream_suppressed" \
      "a notification was sent while a stream was open: $(cat "$WORK/push-received.jsonl")"
  fi
  assertions=$((assertions + 1))

  # And without a stream: one message **per subscription** arrives — three rows, three sends — and the receiver's
  # own checks say each is really this machine's. The tail is scoped to the lines this turn added, so the count is
  # a fact about this prompt rather than about the file.
  before="$(wc -l <"$WORK/push-received.jsonl")"
  fetch_body "${name}_prompt2" POST "/api/sessions/$id/prompt" '{"prompt":"spend it again"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt2/no_answer" "POST a second prompt did not answer"
  expect_status "${name}_prompt2" "$(answer "${name}_prompt2")" 202
  wait_for_reason_id "${name}_free_row" "$id" budget_exceeded
  for _ in $(seq 1 200); do
    [ "$(wc -l <"$WORK/push-received.jsonl")" -ge $((before + 3)) ] && break
    sleep 0.1
  done
  sent="$(sed -n "$((before + 1)),\$p" "$WORK/push-received.jsonl")"
  [ -n "$sent" ] ||
    fail "${name}/delivered" \
      "no notification arrived for $id: $(tail -5 "$WORK/push-received.jsonl" 2>/dev/null)"
  # One message per row, and the one `/ok/` endpoint's is the line the six payload checks read.
  check "${name}/count" 3 "$(printf '%s\n' "$sent" | grep -cF "\"session_id\":\"$id\"")"
  check "${name}/paths" 3 \
    "$(printf '%s\n' "$sent" | grep -cE '\"path\":\"/(ok|gone|keyed)/1\"')"
  line="$(printf '%s\n' "$sent" | grep -F '"path":"/ok/1"' | sed -n '1p')"
  [ -n "$line" ] || fail "${name}/ok_line" "the /ok/ endpoint was not delivered to: $sent"
  # The payload is `{host, session_id, kind}` — three keys, and the words spec item 11 names.
  check "${name}/kind" "budget_exceeded" \
    "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["payload"]["kind"])' "$line")"
  check "${name}/host" "local" \
    "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["payload"]["host"])' "$line")"
  check "${name}/keys" "host,kind,session_id" \
    "$(python3 -c 'import json,sys; print(",".join(sorted(json.loads(sys.argv[1])["payload"])))' "$line")"
  # The request's own four facts, as the receiver read them.
  check "${name}/authorized" "True" \
    "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["authorized"])' "$line")"
  check "${name}/jwt_ok" "True" \
    "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["jwt_ok"])' "$line")"
  check "${name}/aud_ok" "True" \
    "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["aud_ok"])' "$line")"
  check "${name}/ttl" "3600" \
    "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["ttl"])' "$line")"
  check "${name}/encoding" "aes128gcm" \
    "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["encoding"])' "$line")"

  # 3. The disposition table at the far end, which the previous arm already exercised: `/ok/` answered `201` and
  #    kept its row, `/gone/` answered `410` and lost it, `/keyed/` answered `403` and kept it. The last two
  #    prompts are asserted here, and the fourth one proves the surviving pair is still delivered to — two rows,
  #    two more messages, and neither removed.
  before="$(wc -l <"$WORK/push-received.jsonl")"
  fetch_body "${name}_prompt3" POST "/api/sessions/$id/prompt" '{"prompt":"one more"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt3/no_answer" "POST a third prompt did not answer"
  expect_status "${name}_prompt3" "$(answer "${name}_prompt3")" 202
  wait_for_reason_id "${name}_rows_row" "$id" budget_exceeded
  # The `410` endpoint's row is removed without a request asking for it (the task's own `forget`), which is
  # spec item 11's "dead endpoints are removed".
  for _ in $(seq 1 200); do
    rows="$(python3 - "$data/app.db" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
for (endpoint,) in connection.execute("SELECT endpoint FROM push_subscriptions ORDER BY endpoint"):
    print(endpoint)
connection.close()
PY
)"
    printf '%s\n' "$rows" | grep -qF "$gone_endpoint" || break
    sleep 0.1
  done
  # The dead endpoint's row is gone...
  if printf '%s\n' "$rows" | grep -qF "$gone_endpoint"; then
    fail "${name}/gone" "the 410 endpoint's row is still stored: $rows"
  fi
  assertions=$((assertions + 1))
  # ...and the two others are not: a 201 delivered, and a 403 is a key problem rather than a dead endpoint.
  printf '%s\n' "$rows" | grep -qF "$ok_endpoint" ||
    fail "${name}/kept_ok" "the delivered endpoint's row was removed: $rows"
  assertions=$((assertions + 1))
  printf '%s\n' "$rows" | grep -qF "$keyed_endpoint" ||
    fail "${name}/kept_keyed" "the 403 endpoint's row was removed: $rows"
  assertions=$((assertions + 1))
  # The receiver's own record of those two answers, so "it answered 403" is not the case's opinion — and the
  # count is scoped to the lines the turn that followed the removal added, which is what makes "the gone row was
  # not tried again" a measurement.
  check "${name}/keyed_delivered" 1 \
    "$(printf '%s\n' "$sent" | grep -cF '"/keyed/1"')"
  check "${name}/gone_delivered" 1 \
    "$(printf '%s\n' "$sent" | grep -cF '"/gone/1"')"
  for _ in $(seq 1 200); do
    [ "$(wc -l <"$WORK/push-received.jsonl")" -ge $((before + 2)) ] && break
    sleep 0.1
  done
  check "${name}/surviving_rows" 2 \
    "$(sed -n "$((before + 1)),\$p" "$WORK/push-received.jsonl" | grep -cF "\"session_id\":\"$id\"")"
  check "${name}/gone_untried" 0 \
    "$(sed -n "$((before + 1)),\$p" "$WORK/push-received.jsonl" | grep -cF '"/gone/1"')"

  # 4. The `DELETE`, last, because it is the one that removes a subscription the earlier arms need.
  fetch "${name}_delete" DELETE "/api/push/subscribe?endpoint=$ok_endpoint" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete/no_answer" "DELETE /api/push/subscribe did not answer"
  expect_status "${name}_delete" "$(answer "${name}_delete")" 204
  fetch "${name}_delete2" DELETE "/api/push/subscribe?endpoint=$ok_endpoint" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete2/no_answer" "the second DELETE did not answer"
  expect_status "${name}_delete2" "$(answer "${name}_delete2")" 404

  # The whole table is the case's: it removes what it made and gives the shared boot back, so the next case's
  # `stop_serve` finds a pid (`case_disk_quota`'s idiom).
  kill "$receiver_pid" 2>/dev/null || true
  wait "$receiver_pid" 2>/dev/null || true
  extra_pid=""
  rm -rf "$root"
  stop_serve
  start_serve
}

# the installable app. "Installable" is a browser's judgement and **no gate may have one** (ADR-0046), so
# this case measures the four things a browser would otherwise be the only witness of, as bytes:
#
#   1. the shell names the manifest (and iOS's own PNG link), and `/settings` — the third screen — is that shell;
#   2. `/manifest.webmanifest` is this tree's file, served as a manifest, and carries the fields an install needs;
#   3. `/sw.js` is this tree's worker, served as JavaScript, and is **not** the shell (a `/sw.js` answered by the
#      fallback's shell is a registration every browser refuses, silently);
#   4. every icon the manifest names is the PNG this tree built.
#
# And the story's one server-side claim, which is what makes a subscribe possible at all: `/api/health`'s sixth key
# is this machine's `vapid.toml` `public_key`, byte for byte — and `null` once the file is removed, because the
# answer is read at the moment of asking rather than cached at boot.
#
# The pair is written by hand rather than generated: it only has to *be* a pair for the health answer to carry it
# (`vapid::parse` refuses anything else, and the assertion below is what would catch a bad literal), so this case
# needs no `openssl` and prints **no `SKIP` line at all** — a skip here would fire exactly when the feature was
# deleted, which is this project's P1 (the gate rule).
case_pwa() {
  local name=case_pwa root data config response asset
  local vapid_public='BAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0-P0A'
  local vapid_private='ERERERERERERERERERERERERERERERERERERERERERE'
  local icon
  # Two statements, not one, because every right-hand side of a single `local` expands before either assignment
  # (`case_changes`'s note).
  root="$repo_root/target/verify-pwa-$$"
  data="$root/data"
  config="$root/config.toml"

  stop_serve
  rm -rf "$root"
  mkdir -p "$root/projects" "$data/secrets" || fail "$name/mkdir" "could not create $root"
  chmod 0700 "$data/secrets"
  # The header line `vapid::render` writes, because a reader who opens the file should meet the same sentence.
  printf '# The machine'"'"'s Web Push key pair. `server deploy` writes this once and never rewrites it.\npublic_key = "%s"\nprivate_key = "%s"\n' \
    "$vapid_public" "$vapid_private" >"$data/secrets/vapid.toml" ||
    fail "$name/vapid" "could not write the machine's pair"
  chmod 0600 "$data/secrets/vapid.toml"
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\nprojects_root = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\npreview_listen = "127.0.0.1:0"\n' \
    "$owner" "$root/projects" >"$config"
  start_serve "$config" "$data"

  # 1. The shell: this tree's index.html, and it names the manifest and iOS's icon.
  fetch "$name" GET / "Host: 127.0.0.1" || fail "$name/no_answer" "GET / did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  [ -f web/dist/index.html ] ||
    fail "$name/index_html" "web/dist/index.html is missing: run scripts/build.sh"
  cmp -s "$WORK/responses/$name.body" web/dist/index.html ||
    fail "$name/index_html" "GET / is not this tree's web/dist/index.html"
  assertions=$((assertions + 1))
  for needle in \
    '<link rel="manifest" href="/manifest.webmanifest"' \
    '<link rel="apple-touch-icon" href="/icons/apple-touch-icon-180.png"' \
    'name="apple-mobile-web-app-capable" content="yes"'; do
    grep -qF -- "$needle" "$WORK/responses/$name.body" ||
      fail "$name/shell" "the shell does not carry $needle"
    assertions=$((assertions + 1))
  done

  # 2. The manifest: this tree's file, served as one, with the fields an install reads.
  fetch "${name}_manifest" GET /manifest.webmanifest "Host: 127.0.0.1" ||
    fail "${name}_manifest/no_answer" "GET /manifest.webmanifest did not answer"
  response="$(answer "${name}_manifest")"
  expect_status "${name}_manifest" "$response" 200
  # A manifest served as `application/octet-stream` is a manifest Chrome does not install from, and nothing else
  # in this build would notice.
  expect_header "${name}_manifest" "$response" "Content-Type" "application/manifest+json"
  [ -f web/dist/manifest.webmanifest ] ||
    fail "${name}_manifest/dist" "web/dist/manifest.webmanifest is missing: run scripts/build.sh"
  cmp -s "$WORK/responses/${name}_manifest.body" web/dist/manifest.webmanifest ||
    fail "${name}_manifest/bytes" "the served manifest is not this tree's web/dist/manifest.webmanifest"
  assertions=$((assertions + 1))
  for needle in '"display": "standalone"' '"start_url": "/"' '"scope": "/"' '"sizes": "192x192"' \
    '"sizes": "512x512"' '"purpose": "maskable"' '"type": "image/png"'; do
    grep -qF -- "$needle" "$WORK/responses/${name}_manifest.body" ||
      fail "${name}_manifest/fields" "the manifest does not carry $needle"
    assertions=$((assertions + 1))
  done

  # 3. The worker: this tree's `dist/sw.js`, JavaScript, and **not** the shell. The `cmp` against index.html comes
  # first on purpose: a `/sw.js` answered by the fallback would still be a 200 with `text/html`, and a case that
  # only grepped for a sentence would pass over a page.
  fetch "${name}_worker" GET /sw.js "Host: 127.0.0.1" ||
    fail "${name}_worker/no_answer" "GET /sw.js did not answer"
  response="$(answer "${name}_worker")"
  expect_status "${name}_worker" "$response" 200
  expect_header "${name}_worker" "$response" "Content-Type" "text/javascript; charset=utf-8"
  [ -f web/dist/sw.js ] || fail "${name}_worker/dist" "web/dist/sw.js is missing: run scripts/build.sh"
  cmp -s "$WORK/responses/${name}_worker.body" web/dist/sw.js ||
    fail "${name}_worker/bytes" "the served worker is not this tree's web/dist/sw.js"
  assertions=$((assertions + 1))
  if cmp -s "$WORK/responses/${name}_worker.body" web/dist/index.html; then
    fail "${name}_worker/shell" "/sw.js is the app shell, which no browser will register as a worker"
  fi
  assertions=$((assertions + 1))
  # The four `kind` sentences and the three events, in the bytes the artifact serves: the rules themselves are
  # `web/test/push.test.ts`'s, and this is the evidence that the built worker is the one they describe.
  for needle in \
    'a turn finished' 'a session failed' 'a turn timed out' 'a turn ran out of its budget' \
    'pushsubscriptionchange' 'notificationclick' 'openWindow' 'addEventListener("push"' \
    'there is news about a session' 'notificationData'; do
    grep -qF -- "$needle" "$WORK/responses/${name}_worker.body" ||
      fail "${name}_worker/sentences" "the served worker does not carry $needle"
    assertions=$((assertions + 1))
  done
  # The id rule, as the worker really spells it: a tap may only open a path built from sixteen lowercase hex.
  grep -qF -- '/^[0-9a-f]{16}$/' "$WORK/responses/${name}_worker.body" ||
    fail "${name}_worker/id" "the served worker does not gate a tap on the session id's alphabet"
  assertions=$((assertions + 1))

  # 4. The icons: the manifest's three and iOS's own, each the PNG this tree built.
  for icon in icon-192.png icon-512.png icon-512-maskable.png apple-touch-icon-180.png; do
    fetch "${name}_$icon" GET "/icons/$icon" "Host: 127.0.0.1" ||
      fail "${name}_$icon/no_answer" "GET /icons/$icon did not answer"
    response="$(answer "${name}_$icon")"
    expect_status "${name}_$icon" "$response" 200
    expect_header "${name}_$icon" "$response" "Content-Type" "image/png"
    [ -f "web/dist/icons/$icon" ] ||
      fail "${name}_$icon/dist" "web/dist/icons/$icon is missing: run scripts/build.sh"
    cmp -s "$WORK/responses/${name}_$icon.body" "web/dist/icons/$icon" ||
      fail "${name}_$icon/bytes" "the served $icon is not this tree's web/dist/icons/$icon"
    assertions=$((assertions + 1))
  done

  # 5. `/settings` is the third screen, and it is the same shell: the fallback answers it, so the address bar
  # survives a reload with no route and no rewrite (ADR-0007, ADR-0072 D2).
  fetch "${name}_settings" GET /settings "Host: 127.0.0.1" ||
    fail "${name}_settings/no_answer" "GET /settings did not answer"
  response="$(answer "${name}_settings")"
  expect_status "${name}_settings" "$response" 200
  cmp -s "$WORK/responses/${name}_settings.body" web/dist/index.html ||
    fail "${name}_settings/shell" "/settings is not this tree's web/dist/index.html"
  assertions=$((assertions + 1))

  # 6. The screen itself, as far as a served bundle can be read: the **bundle** (the `<script src>` the shell
  # names, fetched from the artifact and compared with the file) carries every element the screen is made of and
  # the iOS sentence item 11 asks for. A needle is the assignment's own quotes, so deleting the container element
  # cannot leave a sibling's id matching (`case_changes_screen`'s lesson), and every needle is counted.
  asset="$(sed -n 's/.*src="\([^"]*\.js\)".*/\1/p' web/dist/index.html | head -1)"
  [ -n "$asset" ] || fail "$name/asset" "the shell names no script to fetch"
  fetch "${name}_asset" GET "$asset" "Host: 127.0.0.1" ||
    fail "${name}_asset/no_answer" "GET $asset did not answer"
  response="$(answer "${name}_asset")"
  expect_status "${name}_asset" "$response" 200
  cmp -s "$WORK/responses/${name}_asset.body" "web/dist$asset" ||
    fail "${name}_asset/bytes" "the served bundle is not this tree's web/dist$asset"
  assertions=$((assertions + 1))
  # `serviceWorker.register("/sw.js")` is in this list because the first draft of this screen forgot the call, and a
  # manifest without a registration is an installable app a push can never arrive at — a fact no gate can see a
  # browser decide (this story's demo found it).
  #
  # These needles spell `id="…"` while the source spells `machine.id = "settings-machine"`: they match because
  # Vite minifies the app bundle by default, which strips the spaces. That is the opposite of the **worker**'s
  # config, which turns minification off on purpose (ADR-0072 D4) — so turning it off for the app too would red
  # this loop with a message pointing at the wrong thing.
  #
  # `nav-link` is a **class** and no longer an id: the header draws one link per other screen, and an
  # id two elements share is not an id.
  for needle in \
    'id="settings"' 'id="settings-machine"' 'id="settings-state"' 'id="settings-action"' \
    'id="settings-detail"' 'id="settings-install"' 'id="settings-per-machine"' 'id="settings-notice"' \
    'id="nav"' 'nav-link' 'serviceWorker.register("/sw.js")' \
    'iOS delivers notifications only to an app on the home screen: tap Share, then Add to Home Screen, and open Holosmith from the icon' \
    'each machine has its own Web Push key pair, so a phone subscribes on every machine it uses' \
    'notifications are blocked for this app' \
    'this machine has no Web Push key pair' 'applicationServerKey' 'push/subscribe'; do
    grep -qF -- "$needle" "$WORK/responses/${name}_asset.body" ||
      fail "${name}_asset/settings" "the served bundle does not carry $needle"
    assertions=$((assertions + 1))
  done

  # 7. The sixth health key: this machine's pair, byte for byte, and `null` once the file is gone — which is the
  # measurement that a browser could really subscribe, and that two machines cannot agree by accident.
  fetch "${name}_health" GET /api/health "Host: 127.0.0.1" ||
    fail "${name}_health/no_answer" "GET /api/health did not answer"
  response="$(answer "${name}_health")"
  expect_status "${name}_health" "$response" 200
  grep -qF -- "\"application_server_key\":\"$vapid_public\"" "$WORK/responses/${name}_health.body" ||
    fail "${name}_health/key" "the health answer does not carry the machine's own pair: $(cat "$WORK/responses/${name}_health.body")"
  assertions=$((assertions + 1))
  # The private half is never served: it is the one string a browser must not be given.
  if grep -qF -- "$vapid_private" "$WORK/responses/${name}_health.body"; then
    fail "${name}_health/private" "the health answer carries the private key"
  fi
  assertions=$((assertions + 1))
  rm -f "$data/secrets/vapid.toml"
  fetch "${name}_health2" GET /api/health "Host: 127.0.0.1" ||
    fail "${name}_health2/no_answer" "the second GET /api/health did not answer"
  response="$(answer "${name}_health2")"
  expect_status "${name}_health2" "$response" 200
  grep -qF -- '"application_server_key":null' "$WORK/responses/${name}_health2.body" ||
    fail "${name}_health2/key" "a machine without a pair must answer null: $(cat "$WORK/responses/${name}_health2.body")"
  assertions=$((assertions + 1))

  # The whole tree is this case's: it gives the shared boot back, so the next case's `stop_serve` finds a pid.
  rm -rf "$root"
  stop_serve
  start_serve
}

# the host registry. Item 4 gives the registration its rules and item 16 gives the seed its job, and this
# case measures both through the artifact on **its own boot** with a pinned control port — the one shape a row naming
# *this* machine needs, because a seed file is written before the boot and the row has to spell the port the boot will
# use.
#
# What is stood in: a `python3` process that binds **three** TLS listeners on loopback, answering a healthy payload,
# a `302`, and a payload with no `name`. Its certificate comes from a private CA that `SSL_CERT_FILE` points at for
# exactly two `start_serve` calls — the same trick `case_push` owns, and it works only because `hosts::probe` passes
# **no** `-CAfile` (ADR-0071's rule): a machine with a private CA configures `openssl` the way it configures every
# other tool.
#
# What it measures, in items:
#
#   1. item 4's four URL rules and two name rules, each answered with its own code, and a valid MagicDNS name that
#      resolves nowhere stored **`unreachable` with its sentence** rather than refused (item 16's "not removed");
#   2. the seed read at startup: a healthy TLS answer (with its own `version`), a `302` that is impossible to follow
#      (the stand-in records **one** request), a payload with no `name`, and the local row — which is `reachable` with
#      *this build's* version without a round trip, because the merge runs before the listener serves;
#   3. item 4's last bullet: the local row with a live session is `409 host_busy` and `?force=true` removes it;
#   4. "the file wins on conflicts" and "a row the file does not mention survives", over a second boot.
#
# What it cannot measure, and does not pretend to: a real MagicDNS name, a real tailnet host, Tailscale's own
# certificate. This machine has no `tailscaled` and cannot add a 100.64 address without root, so the API's
# `422 not_holosmith` arm is measured in `tests/hosts.rs` — the probe's arms against real listeners, and the mapping
# as a function — rather than through the artifact.
case_hosts() {
  local name=case_hosts root="$repo_root/target/verify-hosts-$$"
  local data="$root/data" config="$root/config.toml" tls="$root/tls" seed="$root/hosts.toml"
  local log="$root/asked.txt" standin_pid="" self_port good redirect nameless
  local response body fresh id
  local api_row="api-made"
  # The guards are first, before `stop_serve`: a case that skipped after stopping the run's server would leave
  # `phase_serve`'s own `stop_serve` with no pid to signal.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to bind a port and stand three hosts in (tests/hosts.rs covers the registry as a client meets it)\n' "$name"
    return 0
  fi
  if ! command -v openssl >/dev/null 2>&1; then
    printf 'SKIP %s: no openssl to make the CA the two https stand-ins need (tests/hosts.rs covers the probe over http)\n' "$name"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$root/projects" "$tls" "$data" || fail "$name/mkdir" "could not create $root"

  # One row's field, read with `python3` (this case needs it anyway): the harness has no `jq` and a `sed` over a
  # five-row list would assert the wrong row the first time the order changed.
  hosts_row_field() {
    python3 - "$1" "$2" "$3" <<'PY'
import json, sys
row = next((row for row in json.load(open(sys.argv[3]))["hosts"] if row["name"] == sys.argv[1]), None)
if row is None:
    print("<absent>")
else:
    value = row[sys.argv[2]]
    print("null" if value is None else value)
PY
  }

  # The CA and a `localhost` certificate with a `DNS:` SAN: `-verify_hostname` matches the name in the certificate,
  # so an IP SAN would not do and the URL has to name `localhost` (case_push's measurement, and the seed is not
  # filtered by the host predicate, so `localhost` in a seed row is legal — ADR-0073 D9).
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$tls/ca.key" -out "$tls/ca.pem" -days 1 \
    -subj /CN=VerifyHostsCA >/dev/null 2>&1 || fail "$name/ca" "openssl req did not write a CA"
  openssl req -newkey rsa:2048 -nodes -keyout "$tls/server.key" -out "$tls/server.csr" \
    -subj /CN=localhost >/dev/null 2>&1 || fail "$name/cert" "openssl req did not write a CSR"
  printf 'subjectAltName=DNS:localhost\n' >"$tls/server.ext"
  openssl x509 -req -in "$tls/server.csr" -CA "$tls/ca.pem" -CAkey "$tls/ca.key" \
    -CAcreateserial -out "$tls/server.pem" -days 1 -extfile "$tls/server.ext" >/dev/null 2>&1 ||
    fail "$name/cert" "openssl x509 did not sign the server certificate"

  # The three stand-ins, one process: `good` answers the health payload, `redirect` answers a `302` and appends one
  # line per request to `$log`, and `nameless` answers a payload without a `name`. Each prints the port the kernel
  # gave it, because the seed has to spell them before the boot.
  cat >"$root/standin.py" <<'PY'
import socket, ssl, sys, threading

cert, key, log = sys.argv[1], sys.argv[2], sys.argv[3]
healthy = (b'{"name":"holosmith","version":"verify","role":"server","uptime_s":0,'
           b'"preview_origin":"http://localhost:8081","application_server_key":null}')
nameless = b'{"version":"verify","role":"server","uptime_s":0}'


def answer(status, body, location=None):
    head = "HTTP/1.1 %s\r\n" % status
    if location:
        head += "Location: %s\r\n" % location
    head += "Content-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body)
    return head.encode() + body


def serve(kind):
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 0))
    server.listen(8)
    print("PORT %s %d" % (kind, server.getsockname()[1]), flush=True)
    while True:
        connection, _ = server.accept()
        try:
            wrapped = context.wrap_socket(connection, server_side=True)
        except Exception:
            connection.close()
            continue
        try:
            wrapped.recv(4096)
            with open(log, "a") as asked:
                asked.write(kind + "\n")
            if kind == "good":
                wrapped.sendall(answer("200 OK", healthy))
            elif kind == "redirect":
                wrapped.sendall(answer("302 Found", b"", "https://elsewhere.invalid/api/health"))
            else:
                wrapped.sendall(answer("200 OK", nameless))
        except Exception:
            pass
        finally:
            try:
                wrapped.shutdown(socket.SHUT_RDWR)
            except Exception:
                pass
            wrapped.close()


for kind in ("good", "redirect", "nameless"):
    threading.Thread(target=serve, args=(kind,), daemon=True).start()
threading.Event().wait()
PY
  : >"$log"
  python3 "$root/standin.py" "$tls/server.pem" "$tls/server.key" "$log" \
    >"$root/standin.out" 2>"$root/standin.err" &
  standin_pid=$!
  # Registered with the trap as well as held here: a `fail` exits the script, and this is the only other thing that
  # would leave three TLS listeners behind on ports the next run could bind.
  extra_pid=$standin_pid
  for _ in $(seq 1 200); do
    [ "$(grep -c '^PORT ' "$root/standin.out" 2>/dev/null || true)" = 3 ] && break
    kill -0 "$standin_pid" 2>/dev/null || break
    sleep 0.05
  done
  good="$(sed -n 's/^PORT good \([0-9]*\)$/\1/p' "$root/standin.out")"
  redirect="$(sed -n 's/^PORT redirect \([0-9]*\)$/\1/p' "$root/standin.out")"
  nameless="$(sed -n 's/^PORT nameless \([0-9]*\)$/\1/p' "$root/standin.out")"
  if [ -z "$good" ] || [ -z "$redirect" ] || [ -z "$nameless" ]; then
    fail "$name/standin" "the stand-in did not bind three ports: $(cat "$root/standin.err")"
  fi
  # The control port is pinned: a row naming this machine has to spell it, and the seed is written before the boot.
  self_port="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"

  printf '[[hosts]]\nname = "thisbox"\nurl = "http://127.0.0.1:%s"\n\n' "$self_port" >"$seed"
  printf '[[hosts]]\nname = "good"\nurl = "https://localhost:%s"\n\n' "$good" >>"$seed"
  printf '[[hosts]]\nname = "redirect"\nurl = "https://localhost:%s"\n\n' "$redirect" >>"$seed"
  printf '[[hosts]]\nname = "nameless"\nurl = "https://localhost:%s"\n\n' "$nameless" >>"$seed"
  printf '[[hosts]]\nname = "nowhere"\nurl = "https://attic.tail1234.ts.net"\n' >>"$seed"
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\nidle_stop_min = 0\nprojects_root = "%s"\nmin_free_gb = 1\ncontrol_origin = "http://127.0.0.1:%s"\nhosts_seed = "%s"\npreview_listen = "127.0.0.1:0"\n' \
    "$owner" "$root/projects" "$self_port" "$seed" >"$config"

  # `SSL_CERT_FILE` is exported for exactly this `start_serve` and unset at once: the probe's `s_client` child
  # inherits it, and it is the only way this machine can trust the stand-in's CA.
  export SSL_CERT_FILE="$tls/ca.pem"
  start_serve "$config" "$data" "$self_port"
  unset SSL_CERT_FILE

  # ---- 1. the seed was read at startup, and every row says what the probe found ---------------------------
  fetch "${name}_list" GET /api/hosts "Host: 127.0.0.1" || fail "${name}_list/no_answer" "GET /api/hosts did not answer"
  response="$(answer "${name}_list")"
  expect_status "${name}_list" "$response" 200
  body="$WORK/responses/${name}_list.body"
  check "$name/list/rows" 5 "$(grep -o '"name":"' "$body" | wc -l | tr -d ' ')"
  # The healthy answer: the row's version is the one the *host* reported, and the transport was TLS.
  check "$name/good/reach" reachable "$(hosts_row_field good reach "$body")"
  check "$name/good/version" verify "$(hosts_row_field good version "$body")"
  check "$name/good/error" null "$(hosts_row_field good last_error "$body")"
  # The local row: `reachable` with this build's version, and no round trip — the merge runs before the listener serves.
  check "$name/self/reach" reachable "$(hosts_row_field thisbox reach "$body")"
  check "$name/self/version" "$version" "$(hosts_row_field thisbox version "$body")"
  # The 302: refused, marked unreachable with the code in its sentence, and asked **once** — that is "redirects are
  # not followed" as a client can see it.
  check "$name/redirect/reach" unreachable "$(hosts_row_field redirect reach "$body")"
  case "$(hosts_row_field redirect last_error "$body")" in
    *302*) assertions=$((assertions + 1)) ;;
    *) fail "$name/redirect/sentence" "the redirect's row does not name the 302: $(hosts_row_field redirect last_error "$body")" ;;
  esac
  check "$name/redirect/asked" 1 "$(grep -c '^redirect$' "$log" || true)"
  # A payload with no name: the same answer as a redirect, and the sentence names what was missing.
  check "$name/nameless/reach" unreachable "$(hosts_row_field nameless reach "$body")"
  case "$(hosts_row_field nameless last_error "$body")" in
    *"missing field \`name\`"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/nameless/sentence" "the nameless row's sentence does not name the field: $(hosts_row_field nameless last_error "$body")" ;;
  esac
  # A MagicDNS name that resolves nowhere: item 16's "fails health ... not removed".
  check "$name/nowhere/reach" unreachable "$(hosts_row_field nowhere reach "$body")"
  case "$(hosts_row_field nowhere last_error "$body")" in
    *attic.tail1234.ts.net*) assertions=$((assertions + 1)) ;;
    *) fail "$name/nowhere/sentence" "the unreachable row's sentence names no host: $(hosts_row_field nowhere last_error "$body")" ;;
  esac

  # ---- 2. item 4's URL and name rules, each with its own code -------------------------------------------
  # Every one of these is refused before any probe runs, which is why they need no reachable host.
  for pair in \
    "scheme|ftp://box.tail1234.ts.net" \
    "host|https://example.com" \
    "loopback|https://localhost" \
    "port|https://box.tail1234.ts.net:22" \
    "port-scheme|http://box.tail1234.ts.net:443" \
    "port-text|https://box.tail1234.ts.net:abc" \
    "port-range|https://box.tail1234.ts.net:99999" \
    "port-empty|https://box.tail1234.ts.net:" \
    "path|https://box.tail1234.ts.net/api"; do
    local label="${pair%%|*}" url="${pair#*|}"
    fetch_body "${name}_url_$label" POST /api/hosts "{\"name\":\"workshop\",\"url\":\"$url\"}" \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "${name}_url_$label/no_answer" "POST /api/hosts ($label) did not answer"
    response="$(answer "${name}_url_$label")"
    expect_status "${name}_url_$label" "$response" 422
    expect_code "${name}_url_$label" "$response" bad_host_url
  done
  for pair in "local|local" "case|Workshop"; do
    local label="${pair%%|*}" host_name="${pair#*|}"
    fetch_body "${name}_name_$label" POST /api/hosts "{\"name\":\"$host_name\",\"url\":\"https://box.tail1234.ts.net\"}" \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "${name}_name_$label/no_answer" "POST /api/hosts ($label) did not answer"
    response="$(answer "${name}_name_$label")"
    expect_status "${name}_name_$label" "$response" 422
    expect_code "${name}_name_$label" "$response" bad_host_name
  done
  # A body with a fourth field is told *that*, not described as malformed.
  fetch_body "${name}_extra" POST /api/hosts \
    "{\"name\":\"workshop\",\"url\":\"https://box.tail1234.ts.net\",\"role\":\"server\"}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_extra/no_answer" "POST /api/hosts did not answer"
  expect_code "${name}_extra" "$(answer "${name}_extra")" unknown_field

  # A valid registration that cannot be reached is **stored**, not refused: item 16's "marked unreachable with the
  # last error, not removed". This row is the one the second boot's file does not mention.
  fetch_body "${name}_ok" POST /api/hosts \
    "{\"name\":\"$api_row\",\"url\":\"https://attic.tail1234.ts.net\"}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_ok/no_answer" "POST /api/hosts did not answer"
  response="$(answer "${name}_ok")"
  expect_status "${name}_ok" "$response" 201
  fetch "${name}_list2" GET /api/hosts "Host: 127.0.0.1" || fail "${name}_list2/no_answer" "GET /api/hosts did not answer"
  check "$name/api/reach" unreachable "$(hosts_row_field "$api_row" reach "$WORK/responses/${name}_list2.body")"

  # ---- 3. the local row with a live session: refused, then forced ---------------------------------------
  fetch_body "${name}_created" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-hosts-1" ||
    fail "${name}_created/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_created")"
  expect_status "${name}_created" "$response" 202
  fetch "${name}_busy" DELETE /api/hosts/thisbox \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_busy/no_answer" "DELETE /api/hosts/thisbox did not answer"
  response="$(answer "${name}_busy")"
  expect_status "${name}_busy" "$response" 409
  expect_code "${name}_busy" "$response" host_busy
  case "$response" in
    *force=true*) assertions=$((assertions + 1)) ;;
    *) fail "$name/busy/force" "the refusal does not name the override: $response" ;;
  esac
  # Nothing was removed by the refusal.
  id="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/${name}_created.body")"
  [ -n "$id" ] || fail "$name/created/id" "the create names no session id: $(cat "$WORK/responses/${name}_created.body")"
  fetch "${name}_list3" GET /api/hosts "Host: 127.0.0.1" || fail "${name}_list3/no_answer" "GET /api/hosts did not answer"
  check "$name/busy/kept" reachable "$(hosts_row_field thisbox reach "$WORK/responses/${name}_list3.body")"
  # A row that does not name this machine has no sessions this machine can count, so it goes without a force.
  fetch "${name}_far" DELETE "/api/hosts/nowhere" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_far/no_answer" "DELETE /api/hosts/nowhere did not answer"
  expect_status "${name}_far" "$(answer "${name}_far")" 204
  # The forced removal of the local row: the row goes, the session stays.
  fetch "${name}_forced" DELETE "/api/hosts/thisbox?force=true" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_forced/no_answer" "the forced DELETE did not answer"
  expect_status "${name}_forced" "$(answer "${name}_forced")" 204
  fetch "${name}_list4" GET /api/hosts "Host: 127.0.0.1" || fail "${name}_list4/no_answer" "GET /api/hosts did not answer"
  check "$name/forced/gone" '<absent>' "$(hosts_row_field thisbox name "$WORK/responses/${name}_list4.body")"
  fresh="$(hosts_row_field "$api_row" name "$WORK/responses/${name}_list4.body")"
  check "$name/forced/api-row" "$api_row" "$fresh"
  # The session the refused delete protected is still there: removing the *row* does not stop it, which is the
  # sentence the 409 gave the operator and what `?force=true` means.
  fetch "${name}_session" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
    fail "${name}_session/no_answer" "GET /api/sessions/$id did not answer"
  expect_status "${name}_session" "$(answer "${name}_session")" 200
  # A name the table does not hold is a `404`, for both of a delete's moments.
  fetch "${name}_missing" DELETE /api/hosts/nobody \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_missing/no_answer" "DELETE /api/hosts/nobody did not answer"
  expect_status "${name}_missing" "$(answer "${name}_missing")" 404
  expect_code "${name}_missing" "$(answer "${name}_missing")" not_found

  # ---- 4. the second boot: the file wins on a name, and a row it does not mention survives --------------
  # `good` now names the *redirect* port and the local row comes back (the file seeds it in again), while `api-made`
  # — the row the API created above — is not in the file at all.
  stop_serve
  printf '[[hosts]]\nname = "thisbox"\nurl = "http://127.0.0.1:%s"\n\n' "$self_port" >"$seed"
  printf '[[hosts]]\nname = "good"\nurl = "https://localhost:%s"\n' "$redirect" >>"$seed"
  export SSL_CERT_FILE="$tls/ca.pem"
  start_serve "$config" "$data" "$self_port"
  unset SSL_CERT_FILE
  fetch "${name}_list5" GET /api/hosts "Host: 127.0.0.1" || fail "${name}_list5/no_answer" "GET /api/hosts did not answer"
  body="$WORK/responses/${name}_list5.body"
  check "$name/merge/file-wins" "https://localhost:$redirect" "$(hosts_row_field good url "$body")"
  check "$name/merge/file-wins-reach" unreachable "$(hosts_row_field good reach "$body")"
  check "$name/merge/self-back" reachable "$(hosts_row_field thisbox reach "$body")"
  check "$name/merge/api-row" "https://attic.tail1234.ts.net" "$(hosts_row_field "$api_row" url "$body")"
  # Five rows: the file's two plus the three it says nothing about — the merge never deletes a row it did not read.
  check "$name/merge/rows" 5 "$(grep -o '"name":"' "$body" | wc -l | tr -d ' ')"

  # The whole boot is the case's: the stand-in goes, the tree goes, and the shared boot comes back, so the next
  # case's `stop_serve` finds a pid.
  kill "$standin_pid" 2>/dev/null || true
  wait "$standin_pid" 2>/dev/null || true
  extra_pid=""
  rm -rf "$root"
  stop_serve
  start_serve
}
# `hub deploy` and `hub host` (spec item 10 §6, the Commands table, item 4).
#
# **The one case in this run that needs no daemon and no image**, which is the story's own headline: "on a Docker-less
# machine". A hub's five steps ask no Docker question (`doctor::run` skips the daemon and git for `Role::Hub`), build
# no image and start no container — so nothing here touches `docker` at all, and `tests/deploy.rs`'s
# `a_hub_deploy_runs_five_steps_and_skips_the_two_a_hub_has_no_work_for` measures the same claim from the inside (it
# asserts an empty build list and an empty daemon-call list). python3 is the harness's, for the reason `case_hosts`
# records twice over: `free_port` picks the ports a row naming this machine has to spell before the boot, and one
# row's field is read out of a JSON answer because this gate has no `jq`. A missing python3 is therefore this case's
# only SKIP line, and the five arms it would have run are `tests/deploy.rs`'s and `tests/hub_cli.rs`'s.
#
# What it measures, in the order a failure reads best:
#
# 1. `hub deploy --dry-run`: the six lines in the spec's order with the spec's numbers, three `needs` and two
#    `skipped` (each naming what a hub has no work for), the tally counting the skips apart, and an install root that
#    does not appear.
# 2. the `--root` run of the hub half: steps 2–6 apply, 7 finds the machine serving, `config.toml` says
#    `role = "hub"`, the unit requires no daemon — and the second run is five `satisfied` and two `skipped`.
# 3. the union: the same command on a **server** config leaves `role = "both"` and a unit that does require the
#    daemon, which is acceptance bullet 4's first half. The second half — "the API is unchanged" — is the stub boot
#    of that tree answering `/api/health` as `both` with `/api/hosts` and `/api/sessions` still there.
# 4. `hub host add`/`list`/`remove` against that machine's **own** listener: a stored row printed as its own line, the
#    API's two registration sentences with no code in front, a removal, and the `409 host_busy` a row naming this
#    machine earns while it has a live session — then `--force`, which removes the row and leaves the session.
# 5. `holosmith status`: `machines <n>` and one `machine <name>` line per row, and `machines 0` once they are gone —
#    the acceptance's last bullet, on the machine whose registry it is.
#
# What it cannot measure: a real unit (there is no systemd), a real tailnet (a deploy no longer configures one —
# ADR-0086 — so the `tailscale` stand-in on `PATH` is only there to prove it is never called), a second machine, and
# `--ssh`/`--binary`.
case_hub_deploy() {
  local name=case_hub_deploy root="$repo_root/target/verify-hub-$$"
  local control preview closed shims log state status line got entry holder_pid id
  local hub_config server_config seed tree hub_tree
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to pick the ports a registry row has to spell and to read one row field (tests/deploy.rs and tests/hub_cli.rs cover every arm)\n' "$name"
    return 0
  fi
  # The shared boot is `phase_serve`'s and this case's machine is its own; it comes back at the end, for the reason
  # `case_hosts` gives: the next case's `stop_serve` has to find a pid.
  stop_serve
  rm -rf "$root"
  mkdir -p "$root"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  closed="$(free_port)" || fail "$name/port" "could not ask the kernel for a third port"
  shims="$WORK/$name.shims"
  log="$WORK/$name.tailscale.log"
  state="$WORK/$name.tailscale.state"
  rm -f "$log" "$state"
  deploy_machine_shims "$shims" "$log" "$state"

  # The machine's own config as `serve`/`status`/`hub host` read it: the data root is the install tree's, because
  # `status` and `hub host` read **the config**, not `serve`'s flags.
  hub_config="$WORK/config-hub.toml"
  server_config="$WORK/config-hub-server.toml"
  tree="$root/tree"
  hub_tree="$root/hub"
  deploy_config "$hub_config" "$control" "$preview" "unix:///var/run/docker.sock" "pi-agent:local" "hub"
  # The server's config is the same machine with a seed: a registry row naming **this** machine's control origin is
  # how a local row exists at all, because `POST /api/hosts` refuses loopback (`host_problem`) while the seed is not
  # filtered by that rule (ADR-0073 D9). It is written before the boot because the boot is what merges it.
  seed="$root/hosts.toml"
  printf '[[hosts]]\nname = "thisbox"\nurl = "http://localhost:%s"\n' "$control" >"$seed"
  deploy_config "$server_config" "$control" "$preview" "unix:///var/run/docker.sock" "pi-agent:local" "server"
  printf 'hosts_seed = "%s"\n' "$seed" >>"$server_config"

  # ---- 1. the dry run: the hub's plan, and nothing written -----------------------------------------
  status=0
  PATH="$shims:$PATH" "$artifact" hub deploy --dry-run --root "$hub_tree" \
    --config "$hub_config" >"$WORK/$name.dry.out" 2>&1 || status=$?
  check "$name/dry/exit" 0 "$status"
  got="$(sed -n 's/^\([1-7]\)\. .*/\1/p' "$WORK/$name.dry.out" | tr -d '\n')"
  check "$name/dry/order" "123456" "$got"
  # Step 1 is a **read** (`doctor --quick`), so a dry run prints its verdict rather than a plan: `satisfied` here
  # means this machine passed the hub's five checks, which is the acceptance's "without the Docker and git checks".
  grep -q '^1\. preflight: satisfied: ' "$WORK/$name.dry.out" ||
    fail "$name/dry/preflight" "the hub's preflight did not pass: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  for entry in files service health; do
    grep -q "^[1-6]\. $entry: needs: " "$WORK/$name.dry.out" ||
      fail "$name/dry/$entry" "no $entry plan in the dry run: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
  done
  # The two a hub has no work for: `skipped` with a reason, never a pass.
  for pair in "images:builds no images" "proxies:no proxies"; do
    entry="${pair%%:*}" reason="${pair#*:}"
    line="$(grep "^[1-6]\. $entry: skipped: " "$WORK/$name.dry.out" || true)"
    [ -n "$line" ] || fail "$name/dry/$entry" "step $entry is not a skip in a hub deploy: $(cat "$WORK/$name.dry.out")"
    case "$line" in
      *"$reason"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/dry/$entry/reason" "the $entry skip does not say why a hub has no work for it: $line" ;;
    esac
  done
  grep -q '^deploy: 6 steps: 1 satisfied, 0 changed, 2 skipped, 0 failed$' "$WORK/$name.dry.out" ||
    fail "$name/dry/tally" "the tally does not count the two skips apart: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  [ ! -e "$hub_tree" ] || fail "$name/dry/root" "the dry run created $hub_tree"
  assertions=$((assertions + 1))
  grep -qx "deploy: role: hub" "$WORK/$name.dry.out" ||
    fail "$name/dry/role" "the tail does not say the machine is a hub: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  grep -qx 'deploy: next: holosmith hub host add <name> <url>' "$WORK/$name.dry.out" ||
    fail "$name/dry/next" "a hub is not told to register a machine: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  # A hub has no key proxy, so `key set` is a next step this machine cannot act on.
  if grep -q '^deploy: next: holosmith key set' "$WORK/$name.dry.out"; then
    fail "$name/dry/next/key" "a hub-only machine was told to store a provider key: $(cat "$WORK/$name.dry.out")"
  fi
  assertions=$((assertions + 1))

  # ---- 2. the install: five steps applied, and a second run that changes nothing -------------------
  # Step 6 waits for `/api/health` on the config's control port and then runs `doctor --full`, so one responder on
  # that port is what makes "this machine is serving" a measurement — `case_server_deploy`'s shape, and the same
  # reason: nothing on this machine starts the unit.
  holder_pid="$(health_responder "$control")"
  for _ in $(seq 1 100); do
    if (exec 3<>/dev/tcp/127.0.0.1/"$control") 2>/dev/null; then break; fi
    sleep 0.05
  done
  status=0
  PATH="$shims:$PATH" "$artifact" hub deploy --root "$hub_tree" \
    --config "$hub_config" >"$WORK/$name.run.out" 2>&1 || status=$?
  check "$name/run/exit" 0 "$status"
  while read -r n entry; do
    grep -q "^$n\. $entry: \(changed\|satisfied\): " "$WORK/$name.run.out" ||
      fail "$name/run/$n" "step $n is neither changed nor satisfied: $(cat "$WORK/$name.run.out")"
    assertions=$((assertions + 1))
  done <<'APPLIED'
2 files
5 service
6 health
APPLIED
  grep -q '^2\. files: changed: ' "$WORK/$name.run.out" ||
    fail "$name/run/files" "step 2 did not create the install tree: $(cat "$WORK/$name.run.out")"
  assertions=$((assertions + 1))
  for pair in "3 images" "4 proxies"; do
    n="${pair%% *}" entry="${pair#* }"
    grep -q "^$n\. $entry: skipped: " "$WORK/$name.run.out" ||
      fail "$name/run/$n" "step $n is not a skip: $(cat "$WORK/$name.run.out")"
    assertions=$((assertions + 1))
  done
  for path in "config.toml 644" "data 755" \
    "data/secrets 700" "data/secrets/keys.toml 600" \
    "data/secrets/vapid.toml 600" ; do
    entry="${path% *}" want="${path##* }"
    check "$name/run/mode/$entry" "$want" "$(stat -c '%a' "$hub_tree/$entry" 2>/dev/null || echo absent)"
  done
  # The file it wrote is the machine that boots next, and a hub's unit requires no daemon: a `hub deploy` exists for
  # the machine that has none, and a unit that named `docker.service` there would never start.
  grep -qx 'role = "hub"' "$hub_tree/config.toml" ||
    fail "$name/run/config/role" "step 2 did not write the role it installed: $(cat "$hub_tree/config.toml")"
  assertions=$((assertions + 1))
  if grep -q 'docker.service' "$hub_tree/holosmith.service"; then
    fail "$name/run/unit" "a hub's unit requires a daemon this machine does not have: $(cat "$hub_tree/holosmith.service")"
  fi
  assertions=$((assertions + 1))
  grep -q '^Description=Holosmith hub$' "$hub_tree/holosmith.service" ||
    fail "$name/run/unit/description" "the unit does not describe the machine it boots: $(cat "$hub_tree/holosmith.service")"
  assertions=$((assertions + 1))
  status=0
  "$artifact" config validate --config "$hub_tree/config.toml" >/dev/null 2>&1 || status=$?
  check "$name/run/config/valid" 0 "$status"
  # The second run: re-running is the whole recovery procedure, so a run that reaches the end has to be idempotent.
  status=0
  PATH="$shims:$PATH" "$artifact" hub deploy --root "$hub_tree" \
    --config "$hub_config" >"$WORK/$name.again.out" 2>&1 || status=$?
  check "$name/again/exit" 0 "$status"
  while read -r n entry; do
    grep -q "^$n\. $entry: satisfied: " "$WORK/$name.again.out" ||
      fail "$name/again/$n" "step $n is not satisfied on the second run: $(cat "$WORK/$name.again.out")"
    assertions=$((assertions + 1))
  done <<'AGAIN'
1 preflight
2 files
5 service
6 health
AGAIN
  grep -q '^deploy: 6 steps: 4 satisfied, 0 changed, 2 skipped, 0 failed$' "$WORK/$name.again.out" ||
    fail "$name/again/tally" "the second run's tally is not five satisfied and two skipped: $(cat "$WORK/$name.again.out")"
  assertions=$((assertions + 1))

  # ---- 3. the union: a server machine deployed as a hub is `both` ----------------------------------
  status=0
  PATH="$shims:$PATH" "$artifact" hub deploy --root "$tree" \
    --config "$server_config" >"$WORK/$name.union.out" 2>&1 || status=$?
  check "$name/union/exit" 0 "$status"
  grep -qx 'role = "both"' "$tree/config.toml" ||
    fail "$name/union/role" "a hub deploy on a server machine did not install both: $(cat "$tree/config.toml")"
  assertions=$((assertions + 1))
  grep -q '^deploy: role: both (.*server.*hub deploy.*)' "$WORK/$name.union.out" ||
    fail "$name/union/line" "the run did not say it reached both: $(cat "$WORK/$name.union.out")"
  assertions=$((assertions + 1))
  # A **dry run** over the same server config says `would be`: the header above it says `deploy: dry run`, nothing is
  # written, and a past-tense "added the hub half" over a config that still says `server` would be the report's first
  # lie (ADR-0056's five words, and ADR-0077 D4's wording).
  status=0
  PATH="$shims:$PATH" "$artifact" hub deploy --dry-run --root "$root/dry-union" \
    --config "$server_config" >"$WORK/$name.uniondry.out" 2>&1 || status=$?
  check "$name/uniondry/exit" 0 "$status"
  grep -qx 'deploy: dry run' "$WORK/$name.uniondry.out" ||
    fail "$name/uniondry/header" "the run does not call itself a dry run: $(cat "$WORK/$name.uniondry.out")"
  assertions=$((assertions + 1))
  grep -qxF -- 'deploy: role: both (would be: this machine'"'"'s config says `server`, and `hub deploy` adds the hub half)' \
    "$WORK/$name.uniondry.out" ||
    fail "$name/uniondry/role" "a dry run claims a role change it did not make: $(cat "$WORK/$name.uniondry.out")"
  assertions=$((assertions + 1))
  [ ! -e "$root/dry-union" ] || fail "$name/uniondry/root" "the dry run created $root/dry-union"
  assertions=$((assertions + 1))

  # `both` runs sessions, so its unit keeps the daemon even though the run that wrote it installed the hub's half.
  grep -qx 'Requires=docker.service' "$tree/holosmith.service" ||
    fail "$name/union/unit" "a both machine's unit dropped the daemon it needs: $(cat "$tree/holosmith.service")"
  assertions=$((assertions + 1))
  kill "$holder_pid" 2>/dev/null || true
  wait "$holder_pid" 2>/dev/null || true
  holder_pid=""

  # ---- 4. the machine booted for real: its API, and `hub host` against its own listener -------------
  # `backend = "stub"` in the config the deploy wrote, so `disk::guarded` is false and this boot runs no preflight
  # and starts no proxy: the only machine in this case that starts anything, and it starts nothing Docker.
  start_serve "$tree/config.toml" "$tree/data" "$control"
  fetch "${name}_health" GET /api/health "Host: 127.0.0.1" || fail "${name}_health/no_answer" "GET /api/health did not answer"
  case "$(answer "${name}_health")" in
    *'"role":"both"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/union/api/role" "the both machine does not report its role: $(answer "${name}_health")" ;;
  esac
  fetch "${name}_hosts" GET /api/hosts "Host: 127.0.0.1" || fail "${name}_hosts/no_answer" "GET /api/hosts did not answer"
  expect_status "${name}_hosts" "$(answer "${name}_hosts")" 200
  fetch "${name}_sessions" GET /api/sessions "Host: 127.0.0.1" || fail "${name}_sessions/no_answer" "GET /api/sessions did not answer"
  expect_status "${name}_sessions" "$(answer "${name}_sessions")" 200
  # The seed's row is this machine's — `hosts::is_local` folds the port and compares the host, and the seed's URL is
  # the control origin verbatim, so the shortcut stored it reachable without a round trip.
  case "$(answer "${name}_hosts")" in
    *'"name":"thisbox","url":"http://localhost:'*'"reach":"reachable"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/union/local-row" "the seed's row is not this machine's own: $(answer "${name}_hosts")" ;;
  esac

  # `add`: the row is stored and printed as the same `machine <name>` line `status` prints. `attic` is a MagicDNS
  # name that resolves nowhere, which is item 16's "marked unreachable with the last error, not removed".
  status=0
  "$artifact" hub host add attic https://attic.tail1234.ts.net --config "$tree/config.toml" \
    >"$WORK/$name.add.out" 2>&1 || status=$?
  check "$name/add/exit" 0 "$status"
  grep -q '^machine attic unreachable, last error: .*attic\.tail1234\.ts\.net' "$WORK/$name.add.out" ||
    fail "$name/add/row" "add did not print the row the API stored: $(cat "$WORK/$name.add.out")"
  assertions=$((assertions + 1))
  # The two registration refusals, with the API's sentence and **no code in front** (ADR-0076 D6): the code names a
  # value the operator just typed, so the sentence is the answer.
  status=0
  "$artifact" hub host add local https://box.tail1234.ts.net --config "$tree/config.toml" \
    >"$WORK/$name.badname.out" 2>&1 || status=$?
  check "$name/badname/exit" 1 "$status"
  # The reserved name's own sentence, and the API's: a CLI that spelled its own would be the fork ADR-0076 D6
  # forbids. `local` rather than `Local` because the alphabet rule fires first on an uppercase spelling.
  grep -qF -- '`name` must not be "local": that is the name this machine'"'"'s own sessions are reached by' \
    "$WORK/$name.badname.out" ||
    fail "$name/badname/sentence" "the CLI did not print the API's own sentence: $(cat "$WORK/$name.badname.out")"
  assertions=$((assertions + 1))
  status=0
  "$artifact" hub host add workshop ftp://box.tail1234.ts.net --config "$tree/config.toml" \
    >"$WORK/$name.badurl.out" 2>&1 || status=$?
  check "$name/badurl/exit" 1 "$status"
  got="$(cat "$WORK/$name.badurl.out")"
  case "$got" in
    'holosmith: hub host: must start with http:// or https://, not "ftp://box.tail1234.ts.net"') assertions=$((assertions + 1)) ;;
    *) fail "$name/badurl/sentence" "the URL refusal is not the API's sentence: $got" ;;
  esac
  if grep -q '^holosmith: hub host: bad_host_\(name\|url\):' "$WORK/$name.badurl.out"; then
    fail "$name/badurl/code" "the CLI put a code in front of a sentence about a value the operator typed: $(cat "$WORK/$name.badurl.out")"
  fi
  assertions=$((assertions + 1))
  # `list` answers the table, in the route's own order, through the same client as the other two.
  status=0
  "$artifact" hub host list --config "$tree/config.toml" >"$WORK/$name.list.out" 2>&1 || status=$?
  check "$name/list/exit" 0 "$status"
  grep -qx 'machines 2' "$WORK/$name.list.out" ||
    fail "$name/list/count" "list does not count the two rows: $(cat "$WORK/$name.list.out")"
  assertions=$((assertions + 1))
  for needle in '^machine attic unreachable, ' '^machine thisbox reachable, version '; do
    grep -q "$needle" "$WORK/$name.list.out" ||
      fail "$name/list/row" "list is missing $needle: $(cat "$WORK/$name.list.out")"
    assertions=$((assertions + 1))
  done

  # ---- 5. `status` reads the same table, and the busy row answers with the API's sentence ------------
  status=0
  "$artifact" status --config "$tree/config.toml" >"$WORK/$name.status.out" 2>&1 || status=$?
  check "$name/status/exit" 0 "$status"
  grep -qx 'machines 2' "$WORK/$name.status.out" ||
    fail "$name/status/count" "status does not report the registry: $(cat "$WORK/$name.status.out")"
  assertions=$((assertions + 1))
  grep -q '^machine attic unreachable, last error: ' "$WORK/$name.status.out" ||
    fail "$name/status/row" "status has no line for a registered machine: $(cat "$WORK/$name.status.out")"
  assertions=$((assertions + 1))
  if grep -q 'NO_REGISTRY\|no hub has registered' "$WORK/$name.status.out"; then
    fail "$name/status/stale" "status still prints the phase-2 sentence: $(cat "$WORK/$name.status.out")"
  fi
  assertions=$((assertions + 1))

  # A remote row goes without a force; the row that names this machine is refused while this machine has a session.
  status=0
  "$artifact" hub host remove attic --config "$tree/config.toml" \
    >"$WORK/$name.remove.out" 2>&1 || status=$?
  check "$name/remove/exit" 0 "$status"
  grep -qx 'removed attic' "$WORK/$name.remove.out" ||
    fail "$name/remove/line" "remove did not say what it removed: $(cat "$WORK/$name.remove.out")"
  assertions=$((assertions + 1))
  fetch_body "${name}_created" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Idempotency-Key: verify-hub-1" ||
    fail "${name}_created/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_created" "$(answer "${name}_created")" 202
  id="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/${name}_created.body")"
  [ -n "$id" ] || fail "$name/created/id" "the create names no session id: $(cat "$WORK/responses/${name}_created.body")"
  status=0
  "$artifact" hub host remove thisbox --config "$tree/config.toml" \
    >"$WORK/$name.busy.out" 2>&1 || status=$?
  check "$name/busy/exit" 1 "$status"
  grep -q '^holosmith: hub host: host_busy: "thisbox" is this machine, and it has 1 live session' "$WORK/$name.busy.out" ||
    fail "$name/busy/sentence" "the busy refusal is not the API's sentence with its code: $(cat "$WORK/$name.busy.out")"
  assertions=$((assertions + 1))
  grep -q 'force=true' "$WORK/$name.busy.out" ||
    fail "$name/busy/flag" "the refusal does not name the override it means: $(cat "$WORK/$name.busy.out")"
  assertions=$((assertions + 1))
  status=0
  "$artifact" hub host remove thisbox --force --config "$tree/config.toml" \
    >"$WORK/$name.forced.out" 2>&1 || status=$?
  check "$name/forced/exit" 0 "$status"
  grep -qx 'removed thisbox' "$WORK/$name.forced.out" ||
    fail "$name/forced/line" "--force did not remove the row: $(cat "$WORK/$name.forced.out")"
  assertions=$((assertions + 1))
  # Removing the row does not stop the session, which is what the refusal's sentence says.
  fetch "${name}_session" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
    fail "${name}_session/no_answer" "GET /api/sessions/$id did not answer"
  expect_status "${name}_session" "$(answer "${name}_session")" 200
  "$artifact" status --config "$tree/config.toml" >"$WORK/$name.status2.out" 2>&1 ||
    fail "$name/status2" "status failed after the removals: $(cat "$WORK/$name.status2.out")"
  grep -qx 'machines 0' "$WORK/$name.status2.out" ||
    fail "$name/status2/empty" "an emptied registry is not a count of zero: $(cat "$WORK/$name.status2.out")"
  assertions=$((assertions + 1))

  # A machine that is not serving has no registry to add to, and the CLI says so instead of faking an answer.
  printf 'role = "hub"\nbackend = "stub"\ncontrol_listen = "127.0.0.1:%s"\ncontrol_origin = "http://localhost:%s"\n' \
    "$closed" "$closed" >"$WORK/$name.closed.toml"
  status=0
  "$artifact" hub host list --config "$WORK/$name.closed.toml" >"$WORK/$name.closed.out" 2>&1 || status=$?
  check "$name/closed/exit" 1 "$status"
  grep -q "^holosmith: hub host: nothing answered at 127.0.0.1:$closed" "$WORK/$name.closed.out" ||
    fail "$name/closed/sentence" "the CLI did not name the address it could not reach: $(cat "$WORK/$name.closed.out")"
  assertions=$((assertions + 1))

  # The whole boot is this case's: the tree goes and the shared boot comes back.
  rm -rf "$root"
  stop_serve
  start_serve
}

# one origin that reaches every machine (spec item 3, item 16 and `Roles`; ADR-0074).
#
# Two artifacts on one box is what "two machines" means here: a `role = "server"` boot on a pinned control port and a
# pinned preview port, and a `role = "hub"` boot whose `hosts.toml` names the server's control origin. **The API half
# needs no daemon** — both boots are `backend = "stub"`, and a `hub` is not asked a Docker question at all — which is
# worth saying because it is what makes this case run everywhere. The preview half needs the daemon and the image
# (a real dev server in a container on the peer, reached through the hub's listener) and prints one SKIP without them.
#
# The seed naming `http://127.0.0.1:<port>` is legal where `POST /api/hosts` would answer `422 bad_host_url`: the
# file is the operator's own and is filtered by `name_problem` and `Origin::parse` only, not by item 4's host rule
# (ADR-0073 D9). That asymmetry is this case's whole licence — do not "fix" it by adding the predicate to the seed.
#
# What it measures, in the order a failure reads best:
#
# 1. the probe measured the peer's **preview** origin from its own health answer (the row's new column);
# 2. a create through the hub, the same create again answering the peer's own row (the `Idempotency-Key` survived),
#    and one row's body compared with the peer's own answer **byte for byte** (`cmp`: `Content-Length` is kept);
# 3. the event stream: the hub's frames decoded and compared with the peer's own, `... | cmp`, plus a
#    `Last-Event-ID` round trip that starts at the next seq;
# 4. six answers in five codes: `local`'s `308`, an unknown name, the registry not relayed, a peer that is
#    gone (`502`), a peer that accepts and never answers (`504`), and a peer whose row is a different spelling of the
#    right origin (the peer's own `403 bad_origin`, relayed — the forwarded refusal surviving the trip);
# 5. a hub-only machine serving its reads and refusing the create (`403 sessions_disabled`, no row);
# 6. the preview hop: a container's dev server through the hub's listener, and a hand-written `101` through it;
# 7. a peer killed mid-stream: the client's socket **ends** (EOF, not a timeout) and the next request is the `502`.
case_forward() {
  local name=case_forward root="$repo_root/target/verify-forward-$$"
  local hub_root="$root/hub" peer_root="$root/peer"
  local hub_config="$hub_root/config.toml" peer_config="$peer_root/config.toml"
  local hub_data="$hub_root/data" peer_data="$peer_root/data"
  local seed="$hub_root/hosts.toml" standin_pid="" response body id id2 label
  local hub_control hub_preview peer_control peer_preview silent_port gone_port image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local rc=0 line fd upstream_port events

  # The guards first, before `stop_serve`: a case that skipped after stopping the run's server would leave
  # `phase_serve`'s own `stop_serve` with no pid to signal. The API half below needs nothing, so only the preview
  # arms are conditional — and they are decided once, here, so no half of this case runs a daemon question twice.
  local preview_arms=1
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    preview_arms=0
  elif ! docker image inspect "$image" >/dev/null 2>&1; then
    preview_arms=0
  fi
  # `python3` is needed by three arms — the silent peer, the two decoded streams and the WebSocket handshake — and by
  # nothing else, so it is a flag rather than an early return: the create, the replay, the byte-for-byte row, four of
  # the refusals and the whole of the mid-stream death have no business skipping because one tool is missing.
  local have_python3=1
  command -v python3 >/dev/null 2>&1 || have_python3=0

  stop_serve
  rm -rf "$root"
  mkdir -p "$peer_root/projects/proj" "$peer_data" "$hub_data" || fail "$name/mkdir" "could not create $root"

  # Four pinned ports: two machines, each with a control listener and a preview listener. A row naming a machine has
  # to spell the port it will bind, and the seed is written before either boot.
  hub_control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  hub_preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  peer_control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  peer_preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  silent_port="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  # A port nothing will listen on: bound and released, so the kernel refuses the connection.
  gone_port="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"

  # The server: stub backend (so a create needs no project on disk and no container), its own two origins pinned so
  # the hub's seed can name them before either boot.
  cat >"$peer_config" <<EOF
role = "server"
backend = "stub"
control_origin = "http://127.0.0.1:$peer_control"
preview_listen = "127.0.0.1:$peer_preview"
preview_origin = "http://127.0.0.1:$peer_preview"
EOF

  # A **silent** peer: it answers `/api/health` (so the row is reachable and carries a preview origin) and holds
  # every other request open without answering — which is the only way to measure the forwarding timeout without
  # waiting for a slow route.
  cat >"$root/silent.py" <<'PY'
import socket, sys

port = int(sys.argv[1])
healthy = (b'{"name":"holosmith","version":"silent","role":"server","uptime_s":0,'
           b'"preview_origin":"http://127.0.0.1:1","application_server_key":null}')
server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", port))
server.listen(8)
print("PORT %d" % port, flush=True)
held = []
while True:
    connection, _ = server.accept()
    request = connection.recv(65536)
    if request.split(b"\r\n")[0].endswith(b"/api/health HTTP/1.1"):
        connection.sendall(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %d\r\n"
            b"Connection: close\r\n\r\n" % len(healthy) + healthy
        )
        connection.close()
    else:
        # Accepted, read, and **never answered**: the socket stays open, so the hub's bound is what ends the wait.
        held.append(connection)
PY
  if [ "$have_python3" -eq 1 ]; then
    python3 "$root/silent.py" "$silent_port" >"$root/silent.out" 2>"$root/silent.err" &
    standin_pid=$!
    extra_pid=$standin_pid
    for _ in $(seq 1 200); do
      grep -q '^PORT ' "$root/silent.out" 2>/dev/null && break
      kill -0 "$standin_pid" 2>/dev/null || break
      sleep 0.05
    done
    [ "$(sed -n 's/^PORT \([0-9]*\)$/\1/p' "$root/silent.out")" = "$silent_port" ] ||
      fail "$name/silent" "the silent peer did not bind $silent_port: $(cat "$root/silent.err")"
  fi

  # The hub's registry: the peer under its registered name, the same peer under a **different spelling** of the same
  # origin (a forwarded mutating call to that row must come back as the peer's own `403 bad_origin`), the silent
  # peer, and a machine that is not there. The hub probes every row at boot.
  cat >"$seed" <<EOF
[[hosts]]
name = "workshop"
url = "http://127.0.0.1:$peer_control"

[[hosts]]
name = "wrongspelling"
url = "http://localhost:$peer_control"

[[hosts]]
name = "silent"
url = "http://127.0.0.1:$silent_port"

[[hosts]]
name = "gone"
url = "http://127.0.0.1:$gone_port"
EOF
  cat >"$hub_config" <<EOF
role = "hub"
backend = "stub"
control_origin = "http://127.0.0.1:$hub_control"
preview_listen = "127.0.0.1:$hub_preview"
preview_origin = "http://127.0.0.1:$hub_preview"
hosts_seed = "$seed"
EOF

  start_peer "$peer_config" "$peer_data" "$peer_control"
  # The hub last, because its merge probes the rows: a hub booted before its server learns no preview origin.
  start_serve "$hub_config" "$hub_data" "$hub_control"

  # Wait for the forwarded session to be `idle`, read through the hub: a create (and a prompt) is answered from the
  # row and provisions in the background, and every stub step costs a fraction of a second.
  forward_wait_idle() {
    local waited=0
    while [ "$waited" -lt 100 ]; do
      fetch "$name-idle" GET "/api/hosts/workshop/sessions/$id" "Host: 127.0.0.1" ||
        fail "$name/idle" "GET /api/hosts/workshop/sessions/$id did not answer"
      case "$(cat "$WORK/responses/$name-idle.body")" in
        *'"status":"idle"'*)
          assertions=$((assertions + 1))
          return 0
          ;;
      esac
      sleep 0.2
      waited=$((waited + 1))
    done
    fail "$name/idle" "the forwarded session never reached idle: $(cat "$WORK/responses/$name-idle.body")"
  }

  # ---- 1. the registry measured the peer's second listener --------------------------------------
  fetch "$name-hosts" GET /api/hosts "Host: 127.0.0.1" || fail "$name/hosts" "GET /api/hosts did not answer"
  expect_status "$name/hosts" "$(answer "$name-hosts")" 200
  # One row's facts, as the JSON the struct writes: the field order is the struct's (name, url, preview, reach), so
  # one needle per row is one assertion per row and no `python3` is needed to find it. The needles are assembled in
  # one variable each because a `case` pattern of the form `*"a":"b"*` is a word shellcheck rightly calls ambiguous.
  body="$(cat "$WORK/responses/$name-hosts.body")"
  local want
  want='"name":"workshop","url":"http://127.0.0.1:'"$peer_control"'","preview":"http://127.0.0.1:'"$peer_preview"'","reach":"reachable"'
  case "$body" in
    *"$want"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/hosts/preview" "the peer's row carries no measured preview origin: $body" ;;
  esac
  want='"name":"silent","url":"http://127.0.0.1:'"$silent_port"'","preview":"http://127.0.0.1:1"'
  case "$body" in
    *"$want"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/hosts/silent" "the silent peer's row lost its preview origin: $body" ;;
  esac
  want='"name":"gone","url":"http://127.0.0.1:'"$gone_port"'","preview":null,"reach":"unreachable"'
  case "$body" in
    *"$want"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/hosts/gone" "a peer that is not there is not one unreachable row: $body" ;;
  esac

  # ---- 2. one session API, two machines ----------------------------------------------------------
  fetch "$name-list-empty" GET /api/hosts/workshop/sessions "Host: 127.0.0.1" ||
    fail "$name/list" "GET /api/hosts/workshop/sessions did not answer"
  expect_status "$name/list" "$(answer "$name-list-empty")" 200
  fetch_body "$name-create" POST /api/hosts/workshop/sessions \
    '{"project":"proj","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Idempotency-Key: verify-forward-1" ||
    fail "$name/create" "POST /api/hosts/workshop/sessions did not answer"
  expect_status "$name/create" "$(answer "$name-create")" 202
  id="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/$name-create.body" | sed -n '1p')"
  [ -n "$id" ] || fail "$name/create" "the forwarded create names no id: $(cat "$WORK/responses/$name-create.body")"

  # The same create again: the peer's replay rule, which only holds if the `Idempotency-Key` travelled.
  fetch_body "$name-replay" POST /api/hosts/workshop/sessions \
    '{"project":"proj","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Idempotency-Key: verify-forward-1" ||
    fail "$name/replay" "the replay did not answer"
  expect_status "$name/replay" "$(answer "$name-replay")" 202
  id2="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/$name-replay.body" | sed -n '1p')"
  check "$name/replay/same_row" "$id" "$id2"

  # One row, fetched through the hub and from the peer directly, compared **byte for byte**: the strongest form of
  # "the hub relays the peer's answer" the API offers, and the reason the forwarding route keeps `Content-Length`.
  # Once it has settled: a `starting` row transitions in the background, and two reads either side of that flip are
  # legitimately different documents.
  forward_wait_idle
  forward_fetch "$peer_port" "$name-direct" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
    fail "$name/direct" "GET /api/sessions/$id from the peer's own listener did not answer"
  fetch "$name-row" GET "/api/hosts/workshop/sessions/$id" "Host: 127.0.0.1" || fail "$name/row" "did not answer"
  expect_status "$name/row" "$(answer "$name-row")" 200
  if ! cmp -s "$WORK/responses/$name-direct.body" "$WORK/responses/$name-row.body"; then
    fail "$name/row/bytes" "the hub's row is not the peer's row: $(cat "$WORK/responses/$name-row.body")"
  fi
  assertions=$((assertions + 1))

  # ---- 3. the event stream, event byte for event byte --------------------------------------------
  # A create is answered from the row it just wrote and provisions in the background, so the turn has to wait for
  # `idle` — polled **through the hub**, which is the relayed row read as the peer's own.
  forward_wait_idle
  fetch_body "$name-prompt" POST "/api/hosts/workshop/sessions/$id/prompt" '{"prompt":"hello through the hub"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" ||
    fail "$name/prompt" "the forwarded prompt did not answer"
  expect_status "$name/prompt" "$(answer "$name-prompt")" 202
  # And the turn has to finish before two reads of the stream can agree: the replay is the whole log, and a turn
  # still running would make the two reads stop at different frames.
  forward_wait_idle
  cat >"$root/stream.py" <<'PY'
import socket, sys, time

port, path, out, last = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]
connection = socket.create_connection(("127.0.0.1", port), timeout=5)
head = "GET %s HTTP/1.1\r\nHost: 127.0.0.1\r\n" % path
if last:
    head += "Last-Event-ID: %s\r\n" % last
head += "Connection: close\r\n\r\n"
connection.sendall(head.encode())
raw = b""
# An idle window rather than a total one: the replay arrives at once, and a stream that pauses longer than this is
# one this comparison cannot use anyway (the two reads must see the same frames).
connection.settimeout(1.5)
deadline = time.time() + 15
while time.time() < deadline:
    try:
        chunk = connection.recv(65536)
    except socket.timeout:
        break
    if not chunk:
        break
    raw += chunk
connection.close()
head, _, body = raw.partition(b"\r\n\r\n")
if b"transfer-encoding: chunked" in head.lower():
    decoded = b""
    complete = True
    while True:
        line, _, rest = body.partition(b"\r\n")
        try:
            size = int(line.split(b";")[0], 16)
        except ValueError:
            # A cut in the middle of a chunk: the read stopped before the peer had written the rest, which is this
            # comparison's failure and not a framing question. Keep what arrived and say so.
            complete = False
            break
        if size == 0 or len(rest) < size:
            complete = complete and size == 0
            decoded += rest[:size]
            break
        decoded += rest[:size]
        body = rest[size + 2 :]
    body = decoded
    if not complete:
        print("partial", flush=True)
        open(out + ".raw", "wb").write(raw)
open(out, "wb").write(body)
frames = [frame for frame in body.split(b"\n\n") if frame.strip()]
ids = [line for frame in frames for line in frame.split(b"\n") if line.startswith(b"id: ")]
print("frames %d" % len(frames))
print("first_id %s" % (ids[0].decode().split(" ", 1)[1] if ids else "-"))
PY
  if [ "$have_python3" -eq 0 ]; then
    printf 'SKIP %s/stream: no python3 to decode the two streams and their chunk framing (tests/forward.rs compares one stream'"'"'s event bytes)\n' "$name"
  else
  python3 "$root/stream.py" "$peer_port" "/api/sessions/$id/events" "$WORK/forward-peer.events" "" \
    >"$WORK/forward-peer.txt" 2>&1 || fail "$name/stream" "the peer's own stream failed: $(cat "$WORK/forward-peer.txt")"
  python3 "$root/stream.py" "$port" "/api/hosts/workshop/sessions/$id/events" "$WORK/forward-hub.events" "" \
    >"$WORK/forward-hub.txt" 2>&1 || fail "$name/stream" "the hub's stream failed: $(cat "$WORK/forward-hub.txt")"
  if ! cmp -s "$WORK/forward-peer.events" "$WORK/forward-hub.events"; then
    fail "$name/stream/bytes" "the hub's event bytes are not the peer's: $(cmp "$WORK/forward-peer.events" "$WORK/forward-hub.events" 2>&1)"
  fi
  assertions=$((assertions + 1))
  events="$(sed -n 's/^frames //p' "$WORK/forward-hub.txt")"
  [ "${events:-0}" -ge 2 ] ||
    fail "$name/stream/frames" "the forwarded stream carried ${events:-0} frames: $(cat "$WORK/forward-hub.txt")"
  # The `Last-Event-ID` round trip: ask for the stream after the first frame's seq and the answer starts one later.
  local first resumed
  first="$(sed -n 's/^first_id //p' "$WORK/forward-hub.txt")"
  [ -n "$first" ] && [ "$first" != "-" ] || fail "$name/stream/first_id" "the stream named no id: $(cat "$WORK/forward-hub.txt")"
  python3 "$root/stream.py" "$port" "/api/hosts/workshop/sessions/$id/events" "$WORK/forward-resume.events" "$first" \
    >"$WORK/forward-resume.txt" 2>&1 || fail "$name/stream/resume" "the resumed stream failed: $(cat "$WORK/forward-resume.txt")"
  resumed="$(sed -n 's/^first_id //p' "$WORK/forward-resume.txt")"
  check "$name/stream/resume_at" "$((first + 1))" "$resumed"
  fi

  # ---- 4. the refusals, one code each ------------------------------------------------------------
  fetch "$name-local" GET /api/hosts/local/sessions "Host: 127.0.0.1" || fail "$name/local" "did not answer"
  expect_status "$name/local" "$(answer "$name-local")" 308
  expect_header "$name/local" "$(answer "$name-local")" "location" "/api/sessions"
  body="$(cat "$WORK/responses/$name-local.body")"
  [ -z "$body" ] || fail "$name/local" "the redirect carried a body: $body"
  assertions=$((assertions + 1))

  label="$name-unknown"
  fetch "$label" GET /api/hosts/nobody/sessions "Host: 127.0.0.1" || fail "$label" "did not answer"
  expect_status "$name/unknown" "$(answer "$label")" 404
  expect_code "$name/unknown" "$(answer "$label")" unknown_host

  label="$name-not-forwarded"
  fetch "$label" GET /api/hosts/workshop/hosts "Host: 127.0.0.1" || fail "$label" "did not answer"
  expect_status "$name/not_forwarded" "$(answer "$label")" 403
  expect_code "$name/not_forwarded" "$(answer "$label")" not_forwarded

  label="$name-gone"
  fetch "$label" GET /api/hosts/gone/sessions "Host: 127.0.0.1" || fail "$label" "did not answer"
  expect_status "$name/gone" "$(answer "$label")" 502
  expect_code "$name/gone" "$(answer "$label")" upstream_unreachable

  # The forwarding timeout: five seconds against a peer that took the request and never answers. This is the one
  # wait in the case, and it is the rule's own number rather than a shortened stand-in.
  if [ "$have_python3" -eq 0 ]; then
    printf 'SKIP %s/timeout: no python3 to stand a peer in that accepts and never answers\n' "$name"
  else
    label="$name-timeout"
    fetch "$label" GET /api/hosts/silent/sessions "Host: 127.0.0.1" || fail "$label" "did not answer"
    expect_status "$name/timeout" "$(answer "$label")" 504
    expect_code "$name/timeout" "$(answer "$label")" upstream_timeout
  fi

  # A row naming the peer by **another spelling** of the same origin: the probe reaches it, and every mutating call
  # comes back as the peer's own `403 bad_origin`, because its guard compares the header as text against its
  # `control_origin` while `hosts::is_local` folds ports. That is the rule ADR-0074 records ("register a machine by
  # the exact origin it reports"), and the forwarded refusal arriving intact is the measurement.
  label="$name-wrong-origin"
  fetch_body "$label" POST "/api/hosts/wrongspelling/sessions/$id/abort" '' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" || fail "$label" "did not answer"
  expect_status "$name/wrong_origin" "$(answer "$label")" 403
  expect_code "$name/wrong_origin" "$(answer "$label")" bad_origin

  # ---- 5. a hub-only machine serves its reads and refuses the create -----------------------------
  fetch "$name-hub-list" GET /api/sessions "Host: 127.0.0.1" || fail "$name/hub_list" "did not answer"
  expect_status "$name/hub_list" "$(answer "$name-hub-list")" 200
  body="$(cat "$WORK/responses/$name-hub-list.body")"
  case "$body" in
    *'"id"'*) fail "$name/hub_list" "the hub listed a session of its own: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  fetch_body "$name-hub-create" POST /api/sessions \
    '{"project":"proj","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Idempotency-Key: verify-forward-2" ||
    fail "$name/hub_create" "did not answer"
  expect_status "$name/hub_create" "$(answer "$name-hub-create")" 403
  expect_code "$name/hub_create" "$(answer "$name-hub-create")" sessions_disabled

  # ---- 6. the preview hop: a container's dev server, and an upgrade, through the hub --------------
  if [ "$preview_arms" -eq 0 ]; then
    printf 'SKIP %s/preview: no Docker daemon or no image %s, so the preview hop is not measured here (the API half ran)\n' "$name" "$image"
  elif [ "$have_python3" -eq 0 ]; then
    printf 'SKIP %s/preview: no python3 for the WebSocket handshake or the container readiness probe (tests/forward.rs drives the peer hop against a stand-in)\n' "$name"
  else
    # The port the *peer's* listener dials inside a container, read from the peer's own config.
    upstream_port="$("$artifact" config show --config "$peer_config" |
      sed -n 's/^preview_upstream_port = \([0-9][0-9]*\)$/\1/p')"
    [ -n "$upstream_port" ] || fail "$name/upstream_port" "config show names no preview_upstream_port"
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
  response.end("hello through two machines at " + request.url + "\n");
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
    local address="" waited=0
    while [ "$waited" -lt 100 ]; do
      address="$(docker inspect "s_$id" \
        --format "{{with index .NetworkSettings.Networks \"agents_$id\"}}{{.IPAddress}}{{end}}" 2>/dev/null || true)"
      [ -z "$address" ] || break
      sleep 0.1
      waited=$((waited + 1))
    done
    [ -n "$address" ] || fail "$name/address" "s_$id has no address on agents_$id"
    # A container answers a second or two after `docker run` returns, and the *host* can reach one on an
    # `--internal` network (V7, measured): asking it directly first is what makes a later refusal about the
    # forwarding rather than about a socket that had not opened yet.
    local up=0 tries=0
    while [ "$tries" -lt 60 ]; do
      if timeout 3 python3 -c "
import http.client, sys
connection = http.client.HTTPConnection(sys.argv[1], int(sys.argv[2]), timeout=2)
connection.request('GET', '/')
print(connection.getresponse().status)
" "$address" "$upstream_port" >"$WORK/forward-container.txt" 2>&1 &&
        grep -qx 200 "$WORK/forward-container.txt"; then
        up=1
        break
      fi
      sleep 0.2
      tries=$((tries + 1))
    done
    [ "$up" -eq 1 ] ||
      fail "$name/container_up" "s_$id never answered on $address:$upstream_port: $(cat "$WORK/forward-container.txt")"
    assertions=$((assertions + 1))

    # The path the hub serves, with the peer's registered name in it: `/workshop/<id>/` reaches the peer as
    # `/local/<id>/` and the container as `/`.
    label="$name-preview"
    preview_fetch "$label" GET "/workshop/$id/" || fail "$label" "did not answer"
    expect_status "$name/preview" "$(answer "$label")" 200
    body="$(cat "$WORK/responses/$label.body")"
    case "$body" in
      *"hello through two machines at /"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/preview" "the container's answer did not arrive through two listeners: $body" ;;
    esac

    # The upgrade: a hand-written handshake whose `Origin` is the **hub's** preview origin, rewritten to the peer's
    # on the way out, and one masked frame each way through both tunnels.
    timeout 20 python3 - "$preview_port" "$id" "$peer_preview_port" >"$WORK/forward-websocket.txt" 2>&1 <<'PYWS' ||
import base64
import os
import socket
import sys

hub_port, session, peer_port = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
key = base64.b64encode(os.urandom(16)).decode()
connection = socket.create_connection(("127.0.0.1", hub_port), timeout=10)
connection.sendall(
    (
        f"GET /workshop/{session}/socket HTTP/1.1\r\n"
        f"Host: 127.0.0.1:{hub_port}\r\n"
        f"Origin: http://127.0.0.1:{hub_port}\r\n"
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
payload = b"ping-through-two-machines"
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
      fail "$name/websocket" "the WebSocket round trip through two listeners failed: $(cat "$WORK/forward-websocket.txt")"
    case "$(sed -n '1p' "$WORK/forward-websocket.txt")" in
      "HTTP/1.1 101 Switching Protocols"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/websocket" "the handshake was not answered 101: $(cat "$WORK/forward-websocket.txt")" ;;
    esac
    if ! grep -qx 'echo ping-through-two-machines' "$WORK/forward-websocket.txt"; then
      fail "$name/websocket" "no frame came back through two listeners: $(cat "$WORK/forward-websocket.txt")"
    fi
    assertions=$((assertions + 1))

    docker rm --force "s_$id" >/dev/null 2>&1 || true
    docker network rm "agents_$id" >/dev/null 2>&1 || true
    docker_session_id=""
  fi

  # ---- 7. a peer that goes away mid-stream -------------------------------------------------------
  # An open stream on a session that has events, then the peer's process is killed. What the client must get is the
  # **close** — a `read` that returns 1 (EOF), not 142 (a timeout) — and the next request must be the `502`.
  exec {fd}<>"/dev/tcp/127.0.0.1/$port" || fail "$name/death" "could not open the stream"
  printf 'GET /api/hosts/workshop/sessions/%s/events HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n' \
    "$id" >&"$fd"
  timeout 2 cat <&"$fd" >/dev/null 2>&1 || true
  kill -9 "$peer_pid" 2>/dev/null || fail "$name/death" "could not kill the peer"
  # Waited here so the shell reaps it (and so its "Killed" line is this `wait`'s, silenced, rather than the case's
  # output), and so the peer's own ending is asserted: 137 is SIGKILL, which is what "the machine went away" means
  # here. `stop_peer`'s graceful arm is not this case's — the peer is already gone by the time it would run.
  local died=0
  wait "$peer_pid" 2>/dev/null || died=$?
  peer_pid=""
  check "$name/death/peer_exit" 137 "$died"
  rc=0
  IFS= read -r -t 8 line <&"$fd" || rc=$?
  exec {fd}<&-
  [ "$rc" -eq 1 ] ||
    fail "$name/death" "the stream did not end when the peer went away: read returned $rc (1 is the close, 142 is a timeout)"
  assertions=$((assertions + 1))
  label="$name-after-death"
  fetch "$label" GET /api/hosts/workshop/sessions "Host: 127.0.0.1" || fail "$label" "did not answer"
  expect_status "$name/after_death" "$(answer "$label")" 502
  expect_code "$name/after_death" "$(answer "$label")" upstream_unreachable

  # What this case made of the machine, it takes back, and the shared boot is put back for every case after it.
  docker rm --force "s_$id" >/dev/null 2>&1 || true
  docker network rm "agents_$id" >/dev/null 2>&1 || true
  docker_session_id=""
  stop_serve
  stop_peer
  if [ -n "$standin_pid" ]; then
    kill "$standin_pid" 2>/dev/null || true
    wait "$standin_pid" 2>/dev/null || true
  fi
  standin_pid=""
  extra_pid=""
  # Two data roots, two configs, the seed, the two python helpers and the event dumps: this case's, and every other
  # boot-owning case removes what it made.
  rm -rf "$root"
  start_serve
}


# the Computers screen (spec Roles, item 4, ADR-0076). The screen is a page, and no gate may run a browser
# — so this case measures **the data the page draws** and **the bytes the artifact ships**, which is the same split
# `case_changes_screen` makes. Two boots on one box (`case_forward`'s shape, and its reason: a row naming a machine
# has to spell the port the boot will bind, so both ports are pinned before either config is written):
#
#   1. the hub's own answer: its role is `hub`, its session list is empty, and a create on it is refused — the three
#      facts `createOffered` rests on ("a hub-only machine shows no new-session affordance for itself");
#   2. `GET /api/hosts` lists both machines, the peer `reachable` and the machine that is not there `unreachable`;
#   3. the peer's **health through the hub's hop** — the role, the version and the uptime the card shows, which no
#      registry row carries (ADR-0076 D1);
#   4. the peer's **sessions through the hub's hop**, before and after a session is created on it: the list the
#      group is drawn from, and the row a count folds over;
#   5. a machine that is not there: its hop answers `502 upstream_unreachable` with a sentence naming it, which is
#      what the card degrades to rather than an empty list (ADR-0076 D3);
#   6. the registration's refusals, at the API the form posts to, with the code **and** the sentence the screen
#      prints verbatim (`bad_host_name`, `bad_host_url`); `not_holosmith` is `case_hosts`' pin against a live
#      listener and `tests/hosts.rs`' otherwise;
#   7. adding and removing a row from the registry: `POST /api/hosts` then `DELETE /api/hosts/{name}`;
#   8. a machine that forwards **nothing** — the peer, a `server` with a registry row of its own — answers its own
#      `404 not_found` for that row's sessions, which is the fact the page's role guard rests on (ADR-0076 D3);
#   9. `/computers` is this tree's app shell byte for byte, and the **served bundle** carries the screen's ids, its
#      classes and its sentences.
#
# What it cannot measure, and does not pretend to: the DOM. A needle in a bundle is not a rendered element — the
# weak half is why each sentence is a `web/test/transcript.test.ts` function with its own test — and the demo is
# what watches the page draw.
# one session list and one notification stream across machines.
#
# Two machines on one box — `case_computers`' two boots, `case_push`'s receiver — and the four values this story adds:
# what a machine calls itself in a payload, which machine a create names, which machine each subscription belongs to,
# and what a machine that is away says per session. The peer is a `server` with `machine_name = "workshop"` and a
# budget of one token, so its own stub turn trips `budget_exceeded` and its own pusher signs the notification; the hub
# is a `hub` whose `hosts.toml` names that peer `workshop`, which is the name the tap's path is built from.
#
# What is stood in for the phone: the TLS receiver below, the same one `case_push` runs, with two endpoints and one
# browser key — a real phone mints a *second* subscription key per machine's application server key, and what this
# case measures is the store (two rows, two databases) and the wire (the peer's own pair signs, the peer's own
# endpoint receives), not the browser's key schedule.
#
# What it measures, in the acceptance's own order:
#
#   1. each machine keeps its own subscriptions: a subscribe through the hop writes **one row in the peer's
#      `app.db`** and the hub's own subscribe writes one in the hub's, and the peer's key read through the hop is
#      the peer's own pair;
#   2. the payload's `host` is the peer's own name and its `session_id` is the session the hub created — so a tap
#      opens `/hosts/workshop/sessions/<id>` (`web/test/push.test.ts` asserts the path rule itself);
#   3. a create through the hub names its machine in the **path**, and the peer's own list holds the row;
#   4. the peer stopping is a `502 upstream_unreachable` naming the machine, not an empty list;
#   5. deleting a subscription removes it from the machine that stored it (the page loops; the route pair is what a
#      gate can measure).
#
# It needs `openssl` (the CA, the certificate and the subscription's key), `node` (the receiver) and `python3` (the
# SEC1 walk and two `app.db` reads), and no daemon and no image.
case_cross_machine() {
  local name=case_cross_machine root="$repo_root/target/verify-cross-$$"
  local hub_root="$root/hub" peer_root="$root/peer"
  local hub_config="$hub_root/config.toml" peer_config="$peer_root/config.toml"
  local hub_data="$hub_root/data" peer_data="$peer_root/data"
  local seed="$hub_root/hosts.toml" tls="$root/tls" projects="$peer_root/projects"
  local hub_control hub_preview peer_control peer_preview push_port="" port_line
  local vapid_public vapid_private subscription_public subscription_private_hex subscription_public_hex
  local auth_b64 auth_hex response body id peer_endpoint hub_endpoint rows received try

  if ! command -v openssl >/dev/null 2>&1; then
    printf 'SKIP %s: no openssl to make the receiver certificate and the subscription key (tests/push.rs covers the payload)\n' "$name"
    return 0
  fi
  if ! command -v node >/dev/null 2>&1; then
    printf 'SKIP %s: no node to run the TLS receiver (tests/push.rs covers the payload keys)\n' "$name"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to ask for four ports, walk the SEC1 keys and read two app.db files\n' "$name"
    return 0
  fi

  stop_serve
  rm -rf "$root"
  mkdir -p "$projects/holosmith" "$peer_data/secrets" "$hub_data/secrets" "$tls" ||
    fail "$name/mkdir" "could not create $root"
  chmod 0700 "$peer_data/secrets" "$hub_data/secrets"
  git -C "$projects/holosmith" init --quiet -b main . || fail "$name/git" "git init did not work"
  git -C "$projects/holosmith" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work"

  hub_control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  hub_preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  peer_control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  peer_preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"

  # The peer's pair is **real** — `ecparam`'s own point, walked in `write_sec1_walk` — because the receiver verifies
  # the token against it and node validates the point (`case_pwa`'s hand-written literal is a *shape*, which is all
  # that case needs and not enough for this one: `ERR_CRYPTO_INVALID_JWK` is what that difference looks like). The
  # hub's is the literal, since nothing signs with it here.
  openssl ecparam -name prime256v1 -genkey -noout -outform DER -out "$tls/vapid.der" ||
    fail "$name/ecparam" "openssl ecparam did not write a key"
  write_sec1_walk
  read -r vapid_public vapid_private _ _ < <(python3 "$WORK/sec1.py" "$tls/vapid.der")
  [ -n "$vapid_public" ] && [ -n "$vapid_private" ] ||
    fail "$name/vapid" "the generated pair did not parse"
  printf 'public_key = "%s"\nprivate_key = "%s"\n' "$vapid_public" "$vapid_private" \
    >"$peer_data/secrets/vapid.toml" || fail "$name/vapid" "could not write the peer's pair"
  printf 'public_key = "%s"\nprivate_key = "%s"\n' \
    'BAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0-P0E' \
    'IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI' >"$hub_data/secrets/vapid.toml" ||
    fail "$name/vapid" "could not write the hub's pair"
  chmod 0600 "$peer_data/secrets/vapid.toml" "$hub_data/secrets/vapid.toml"

  # The subscription's own pair: the browser's, and only the receiver can use the private half.
  openssl ecparam -name prime256v1 -genkey -noout -outform DER -out "$tls/subscription.der" ||
    fail "$name/ecparam" "openssl ecparam did not write the subscription's key"
  read -r subscription_public _ subscription_private_hex subscription_public_hex \
    < <(python3 "$WORK/sec1.py" "$tls/subscription.der")
  [ -n "$subscription_public" ] && [ -n "$subscription_public_hex" ] ||
    fail "$name/subscription" "the subscription's key did not parse"
  auth_hex="$(openssl rand -hex 16)"
  auth_b64="$(printf '%s' "$auth_hex" | python3 -c \
    'import base64, sys; print(base64.urlsafe_b64encode(bytes.fromhex(sys.stdin.read())).rstrip(b"=").decode())')"
  [ -n "$auth_b64" ] || fail "$name/auth" "openssl rand did not answer 16 octets"

  # The TLS material: a CA and a `localhost` certificate with a `DNS:` SAN (`-verify_hostname` matches the SAN, not
  # the CN — the landmine).
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$tls/ca.key" -out "$tls/ca.pem" -days 1 \
    -subj /CN=VerifyCrossCA >/dev/null 2>&1 || fail "$name/ca" "openssl req did not write a CA"
  openssl req -newkey rsa:2048 -nodes -keyout "$tls/server.key" -out "$tls/server.csr" \
    -subj /CN=localhost >/dev/null 2>&1 || fail "$name/cert" "openssl req did not write a CSR"
  printf 'subjectAltName=DNS:localhost\n' >"$tls/server.ext"
  openssl x509 -req -in "$tls/server.csr" -CA "$tls/ca.pem" -CAkey "$tls/ca.key" \
    -CAcreateserial -out "$tls/server.pem" -days 1 -extfile "$tls/server.ext" >/dev/null 2>&1 ||
    fail "$name/cert" "openssl x509 did not sign the server certificate"

  # The two machines. The peer's own name is the value this story adds, and its budget of one token is what makes its
  # stub turn end with `budget_exceeded` — the trigger that needs no clock and no real agent.
  cat >"$peer_config" <<EOF
role = "server"
backend = "stub"
machine_name = "workshop"
control_origin = "http://127.0.0.1:$peer_control"
preview_listen = "127.0.0.1:$peer_preview"
preview_origin = "http://127.0.0.1:$peer_preview"
projects_root = "$projects"
max_live_sessions = 2
max_session_tokens = 1
idle_stop_min = 0
min_free_gb = 1
EOF
  cat >"$seed" <<EOF
[[hosts]]
name = "workshop"
url = "http://127.0.0.1:$peer_control"
EOF
  cat >"$hub_config" <<EOF
role = "hub"
backend = "stub"
control_origin = "http://127.0.0.1:$hub_control"
preview_listen = "127.0.0.1:$hub_preview"
preview_origin = "http://127.0.0.1:$hub_preview"
hosts_seed = "$seed"
idle_stop_min = 0
min_free_gb = 1
EOF

  write_push_receiver
  rm -f "$WORK/cross-received.jsonl"
  : >"$WORK/cross-received.jsonl"
  # The receiver's seven arguments are `case_push`'s: the certificate, its key, the log, the **peer's** application
  # server key (the machine that signs), the subscription's private and public halves, and the `auth` secret the row
  # was stored with — the same one, because this case subscribes once and never rotates it.
  node "$WORK/push-receiver.mjs" "$tls/server.pem" "$tls/server.key" "$WORK/cross-received.jsonl" \
    "$vapid_public" "$subscription_private_hex" "$subscription_public_hex" "$auth_hex" \
    >"$WORK/cross-receiver.out" 2>"$WORK/cross-receiver.err" &
  local receiver_pid=$!
  extra_pid=$receiver_pid
  for _ in $(seq 1 100); do
    port_line="$(sed -n 's/^PORT \([0-9]*\)$/\1/p' "$WORK/cross-receiver.out" | sed -n '1p')"
    [ -n "$port_line" ] && break
    kill -0 "$receiver_pid" 2>/dev/null || break
    sleep 0.05
  done
  if [ -z "$port_line" ]; then
    kill "$receiver_pid" 2>/dev/null || true
    fail "$name/receiver" "the push receiver did not bind: $(cat "$WORK/cross-receiver.err")"
  fi
  push_port="$port_line"
  peer_endpoint="https://localhost:$push_port/peer/1"
  hub_endpoint="https://localhost:$push_port/hub/1"

  # The peer first: its sender is the machine that has to trust a private CA (`SSL_CERT_FILE`, exported for exactly
  # this boot — the sender passes no `-CAfile` on purpose, ADR-0071). The hub second, because its registry probes the
  # seeded row at startup.
  export SSL_CERT_FILE="$tls/ca.pem"
  start_peer "$peer_config" "$peer_data" "$peer_control"
  unset SSL_CERT_FILE
  start_serve "$hub_config" "$hub_data" "$hub_control"

  # ---- 1. each machine's own name and its own key, read through the hop ---------------------------
  fetch "$name-hub-health" GET /api/health "Host: 127.0.0.1" || fail "$name/hub-health" "GET /api/health did not answer"
  expect_status "$name-hub-health" "$(answer "$name-hub-health")" 200
  body="$(cat "$WORK/responses/$name-hub-health.body")"
  case "$body" in
    *'"machine_name":"local"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/hub-health/name" "a machine with no name of its own is local: $body" ;;
  esac

  fetch "$name-peer-health" GET /api/hosts/workshop/health "Host: 127.0.0.1" \
    "Origin: $control_origin" "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/peer-health" "GET /api/hosts/workshop/health did not answer"
  expect_status "$name/peer-health" "$(answer "$name-peer-health")" 200
  body="$(cat "$WORK/responses/$name-peer-health.body")"
  # The peer's **own** name, and its **own** pair: this is the value the page subscribes with, one hop out.
  case "$body" in
    *'"machine_name":"workshop"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/peer-health/name" "the peer's health through the hop does not name it workshop: $body" ;;
  esac
  case "$body" in
    *"\"application_server_key\":\"$vapid_public\""*) assertions=$((assertions + 1)) ;;
    *) fail "$name/peer-health/key" "the hop does not carry the peer's own pair: $body" ;;
  esac

  # ---- 2. the subscriptions: one per machine, each in that machine's own table --------------------
  fetch_body "$name-peer-sub" POST /api/hosts/workshop/push/subscribe \
    "{\"endpoint\":\"$peer_endpoint\",\"p256dh\":\"$subscription_public\",\"auth\":\"$auth_b64\"}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/peer-sub" "POST /api/hosts/workshop/push/subscribe did not answer"
  expect_status "$name/peer-sub" "$(answer "$name-peer-sub")" 204
  fetch_body "$name-hub-sub" POST /api/push/subscribe \
    "{\"endpoint\":\"$hub_endpoint\",\"p256dh\":\"$subscription_public\",\"auth\":\"$auth_b64\"}" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/hub-sub" "POST /api/push/subscribe did not answer"
  expect_status "$name-hub-sub" "$(answer "$name-hub-sub")" 204
  rows="$(python3 - "$peer_data/app.db" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
print(" ".join(row[0] for row in connection.execute("SELECT endpoint FROM push_subscriptions ORDER BY endpoint")))
connection.close()
PY
)"
  check "$name/peer-row" "$peer_endpoint" "$rows"
  rows="$(python3 - "$hub_data/app.db" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
print(" ".join(row[0] for row in connection.execute("SELECT endpoint FROM push_subscriptions ORDER BY endpoint")))
connection.close()
PY
)"
  # The hub's own table has the hub's own subscription and **not** the peer's: a subscription is stored by the
  # machine that will sign for it, which is what "each machine keeps its own subscriptions" means.
  check "$name/hub-row" "$hub_endpoint" "$rows"

  # ---- 3. a create through the hub names its machine in the path ---------------------------------
  fetch_body "$name-create" POST /api/hosts/workshop/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-cross-1" ||
    fail "$name-create" "POST /api/hosts/workshop/sessions did not answer"
  expect_status "$name-create" "$(answer "$name-create")" 202
  body="$(cat "$WORK/responses/$name-create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "$name-create/id" "the forwarded create names no id: $body"
  # The row is the peer's own: the peer's own route answers it, which is the fact a hub's list cannot invent.
  for try in $(seq 1 100); do
    forward_fetch "$peer_control" "$name-peer-row" GET "/api/sessions/$id" \
      "Host: 127.0.0.1" "Origin: http://127.0.0.1:$peer_control" ||
      fail "$name/peer-row" "GET /api/sessions/$id on the peer did not answer"
    body="$(cat "$WORK/responses/$name-peer-row.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  expect_status "$name-peer-row" "$(answer "$name-peer-row")" 200
  case "$body" in
    *'"status":"idle"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/peer-row/status" "the session did not provision on the peer: $body" ;;
  esac

  # ---- 4. a turn on the peer notifies the receiver with the peer's name --------------------------
  fetch_body "$name-prompt" POST "/api/hosts/workshop/sessions/$id/prompt" \
    '{"prompt":"go"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name-prompt" "POST /api/hosts/workshop/sessions/$id/prompt did not answer"
  expect_status "$name-prompt" "$(answer "$name-prompt")" 202
  received=""
  for try in $(seq 1 300); do
    received="$(python3 - "$WORK/cross-received.jsonl" <<'PY'
import json, sys
lines = []
for line in open(sys.argv[1]):
    record = json.loads(line)
    payload = record.get("payload")
    if isinstance(payload, dict) and payload.get("session_id"):
        lines.append("%s|%s|%s" % (payload.get("host"), payload.get("session_id"), payload.get("kind")))
print(",".join(lines))
PY
)"
    [ -n "$received" ] && break
    sleep 0.1
  done
  # The payload's `host` is the peer's own name and the id is the session the **hub** created, so the tap's path is
  # `/hosts/workshop/sessions/<id>` — one machine's session, opened from the hub's origin.
  check "$name/payload" "workshop|$id|budget_exceeded" "$received"

  # ---- 5. the peer going away is a sentence per session, not an empty list ------------------------
  stop_peer
  fetch "$name-gone" GET /api/hosts/workshop/sessions "Host: 127.0.0.1" \
    "Origin: $control_origin" "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/gone" "GET /api/hosts/workshop/sessions did not answer"
  expect_status "$name-gone" "$(answer "$name-gone")" 502
  body="$(cat "$WORK/responses/$name-gone.body")"
  case "$body" in
    *'"code":"upstream_unreachable"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/gone/code" "an unreachable machine is not upstream_unreachable: $body" ;;
  esac
  case "$body" in
    *"workshop"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/gone/sentence" "the refusal does not name the machine: $body" ;;
  esac

  # ---- 6. the hub's own create is still refused, and a delete still reaches the machine ----------
  # The decision this story took: a create names its machine in the **path**, so the hub's own route keeps its
  # `403 sessions_disabled` rather than becoming a proxy (ADR-0074 D3, ADR-0079 D2).
  fetch_body "$name-hub-create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-cross-2" ||
    fail "$name-hub-create" "POST /api/sessions on the hub did not answer"
  expect_status "$name-hub-create" "$(answer "$name-hub-create")" 403
  body="$(cat "$WORK/responses/$name-hub-create.body")"
  case "$body" in
    *'"code":"sessions_disabled"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name-hub-create/code" "the hub's own create is not sessions_disabled: $body" ;;
  esac

  # The page's delete loops every machine; what a gate can measure is the route pair it loops over. The peer goes up
  # again for it, because the arm that matters is the one that really removes a row.
  start_peer "$peer_config" "$peer_data" "$peer_control"
  fetch "$name-delete-peer" DELETE \
    "/api/hosts/workshop/push/subscribe?endpoint=$(printf '%s' "$peer_endpoint" | python3 -c \
      'import sys, urllib.parse; print(urllib.parse.quote(sys.stdin.read(), safe=""))')" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/delete-peer" "DELETE on the peer did not answer"
  expect_status "$name-delete-peer" "$(answer "$name-delete-peer")" 204
  rows="$(python3 - "$peer_data/app.db" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
print(str(connection.execute("SELECT count(*) FROM push_subscriptions").fetchone()[0]))
connection.close()
PY
)"
  check "$name/peer-deleted" "0" "$rows"
  # A mutating request carries `Content-Type` even with no body (ADR-0010's `bad_client` arm, and the page's own
  # `JSON_HEADERS` does the same).
  fetch "$name-delete-hub" DELETE "/api/push/subscribe?endpoint=$hub_endpoint" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/delete-hub" "DELETE on the hub did not answer"
  expect_status "$name-delete-hub" "$(answer "$name-delete-hub")" 204

  # The two machines are this case's, and the shared boot goes back for the cases after it.
  stop_peer
  rm -rf "$root"
  stop_serve
  start_serve
}

case_computers() {
  local name=case_computers root="$repo_root/target/verify-computers-$$"
  local hub_root="$root/hub" peer_root="$root/peer"
  local hub_config="$hub_root/config.toml" peer_config="$peer_root/config.toml"
  local hub_data="$hub_root/data" peer_data="$peer_root/data"
  local seed="$hub_root/hosts.toml" peer_seed="$peer_root/hosts.toml"
  local hub_control hub_preview peer_control peer_preview gone_port
  local response body id asset version want needle

  # The guard comes first, before `stop_serve`: a skipped case that stopped the run's server leaves the phase's own
  # teardown with no pid to signal. `free_port` is `python3`'s, and pinned ports are the whole reason this case can
  # name one machine from another machine's config.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to ask the kernel for the four pinned ports this case needs (web/test/transcript.test.ts covers the screen'"'"'s rules)\n' "$name"
    return 0
  fi

  stop_serve
  rm -rf "$root"
  mkdir -p "$peer_root/projects/proj" "$peer_data" "$hub_data" || fail "$name/mkdir" "could not create $root"

  # Four pinned ports and one nothing listens on: two machines, each with a control and a preview listener, and the
  # row that stands in for a machine that is away.
  hub_control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  hub_preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  peer_control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  peer_preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  gone_port="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"

  # The peer is a `server` with a registry of its own named by a seed, because a seed is copied to **every** machine
  # so the lists stay aligned — and that row is what makes the last arm measurable: a machine that forwards nothing
  # answers its own `404 not_found` for another machine's sessions (ADR-0076 D3). The hub is not up yet when the peer
  # probes, so the row is born unreachable, which is a fact about this case's order and not about the code.
  cat >"$peer_seed" <<EOF
[[hosts]]
name = "hub"
url = "http://127.0.0.1:$hub_control"
EOF
  cat >"$peer_config" <<EOF
role = "server"
backend = "stub"
control_origin = "http://127.0.0.1:$peer_control"
preview_listen = "127.0.0.1:$peer_preview"
preview_origin = "http://127.0.0.1:$peer_preview"
hosts_seed = "$peer_seed"
EOF

  # The hub's registry: the peer under the name the screen will show it as, and a machine that is not there. Both
  # are `http://127.0.0.1`, which `POST /api/hosts` would refuse and the seed does not filter — the one asymmetry
  # that lets two machines be measured on one box (ADR-0073 D9).
  cat >"$seed" <<EOF
[[hosts]]
name = "workshop"
url = "http://127.0.0.1:$peer_control"

[[hosts]]
name = "gone"
url = "http://127.0.0.1:$gone_port"
EOF

  cat >"$hub_config" <<EOF
role = "hub"
backend = "stub"
control_origin = "http://127.0.0.1:$hub_control"
preview_listen = "127.0.0.1:$hub_preview"
preview_origin = "http://127.0.0.1:$hub_preview"
hosts_seed = "$seed"
EOF

  start_peer "$peer_config" "$peer_data" "$peer_control"
  # The hub last, because its merge probes the rows (ADR-0073): a hub booted first would learn no preview origin.
  start_serve "$hub_config" "$hub_data" "$hub_control"

  # ---- 1. the hub's own answer: role, no sessions, and a refused create ---------------------------
  fetch "$name-health" GET /api/health "Host: 127.0.0.1" || fail "$name/health" "GET /api/health did not answer"
  expect_status "$name/health" "$(answer "$name-health")" 200
  body="$(cat "$WORK/responses/$name-health.body")"
  case "$body" in
    *'"role":"hub"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/health/role" "the hub does not report its own role: $body" ;;
  esac
  # The version every card compares against, read from the hub's own answer rather than from `version --json`: both
  # machines run this build, so a skew here would be a test that could not fail.
  version="$(printf '%s\n' "$body" | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')"
  [ -n "$version" ] || fail "$name/health/version" "the health answer names no version: $body"

  fetch "$name-own" GET /api/sessions "Host: 127.0.0.1" || fail "$name/own" "GET /api/sessions did not answer"
  expect_status "$name/own" "$(answer "$name-own")" 200
  body="$(cat "$WORK/responses/$name-own.body")"
  case "$body" in
    *'"sessions":[]'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/own/empty" "a hub holds no sessions of its own: $body" ;;
  esac
  # The fact the affordance rule rests on: a hub refuses a create, so a Start button on one is a button whose only
  # answer is this refusal (ADR-0074).
  fetch_body "$name-hub-create" POST /api/sessions \
    '{"project":"proj","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Idempotency-Key: verify-computers-hub" ||
    fail "$name/hub_create" "POST /api/sessions on the hub did not answer"
  expect_status "$name/hub_create" "$(answer "$name-hub-create")" 403
  expect_code "$name/hub_create" "$(answer "$name-hub-create")" sessions_disabled

  # ---- 2. the registry: two machines, one of them away -------------------------------------------
  fetch "$name-hosts" GET /api/hosts "Host: 127.0.0.1" || fail "$name/hosts" "GET /api/hosts did not answer"
  expect_status "$name/hosts" "$(answer "$name-hosts")" 200
  body="$(cat "$WORK/responses/$name-hosts.body")"
  # One needle per row, assembled in a variable because a `case` pattern of the form `*"a":"b"*` is ambiguous to a
  # reader. The field order is the struct's, so a needle can stop before the varying `last_checked_at`.
  want='"name":"workshop","url":"http://127.0.0.1:'"$peer_control"'","preview":"http://127.0.0.1:'"$peer_preview"'","reach":"reachable"'
  case "$body" in
    *"$want"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/hosts/peer" "the peer's row is not reachable with its measured preview origin: $body" ;;
  esac
  want='"name":"gone","url":"http://127.0.0.1:'"$gone_port"'","preview":null,"reach":"unreachable"'
  case "$body" in
    *"$want"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/hosts/gone" "a machine that is not there is one unreachable row: $body" ;;
  esac

  # ---- 3. the peer's health, through the hub's hop -----------------------------------------------
  # What the card's role, version and uptime are read from: the *health payload*, because a registry row carries
  # none of the three (ADR-0076 D1).
  fetch "$name-peer-health" GET /api/hosts/workshop/health "Host: 127.0.0.1" ||
    fail "$name/peer_health" "GET /api/hosts/workshop/health did not answer"
  expect_status "$name/peer_health" "$(answer "$name-peer-health")" 200
  body="$(cat "$WORK/responses/$name-peer-health.body")"
  for needle in '"role":"server"' "\"version\":\"$version\"" '"uptime_s":'; do
    case "$body" in
      *"$needle"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/peer_health/fact" "the peer's health answer does not carry $needle: $body" ;;
    esac
  done

  # ---- 4. the peer's sessions, before and after one exists ---------------------------------------
  # A machine with no sessions still answers: an empty list is a fact, and the card's counts fold over it.
  fetch "$name-peer-empty" GET /api/hosts/workshop/sessions "Host: 127.0.0.1" ||
    fail "$name/peer_empty" "GET /api/hosts/workshop/sessions did not answer"
  expect_status "$name/peer_empty" "$(answer "$name-peer-empty")" 200
  body="$(cat "$WORK/responses/$name-peer-empty.body")"
  case "$body" in
    *'"sessions":[]'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/peer_empty/list" "the peer starts with no sessions: $body" ;;
  esac

  # A session created **on the peer, through the hub**: the group the Sessions screen draws for that machine, and
  # the row whose `status` the count is a fold over.
  fetch_body "$name-create" POST /api/hosts/workshop/sessions \
    '{"project":"proj","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Idempotency-Key: verify-computers-1" ||
    fail "$name/create" "POST /api/hosts/workshop/sessions did not answer"
  expect_status "$name/create" "$(answer "$name-create")" 202
  id="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/$name-create.body" | sed -n '1p')"
  [ -n "$id" ] || fail "$name/create" "the forwarded create names no id: $(cat "$WORK/responses/$name-create.body")"

  # The stub walks its four steps in the background, so the list is read until it says `idle` — through the hub, which
  # is the route the screen itself reads.
  for _ in $(seq 1 100); do
    fetch "$name-peer-list" GET /api/hosts/workshop/sessions "Host: 127.0.0.1" ||
      fail "$name/peer_list" "GET /api/hosts/workshop/sessions did not answer"
    body="$(cat "$WORK/responses/$name-peer-list.body")"
    case "$body" in
      *'"status":"idle"'*) break ;;
    esac
    sleep 0.1
  done
  expect_status "$name/peer_list" "$(answer "$name-peer-list")" 200
  case "$body" in
    *"\"id\":\"$id\""*) assertions=$((assertions + 1)) ;;
    *) fail "$name/peer_list/row" "the session created on the peer is not in the peer's list: $body" ;;
  esac
  case "$body" in
    *'"status":"idle"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/peer_list/idle" "the peer's session never reached idle: $body" ;;
  esac
  # The machine's own window, not the hub's: the group's rows count down against the answer that carried them.
  case "$body" in
    *'"stopped_ttl_days":'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/peer_list/ttl" "the peer's list carries no stopped window: $body" ;;
  esac

  # ---- 5. a machine that is not there -------------------------------------------------------------
  # The hub answers for a peer it cannot reach: `502` and a sentence naming the machine and its origin. That is what
  # the card degrades to — never an empty list, which would read as a machine with nothing on it (ADR-0076 D3).
  fetch "$name-gone" GET /api/hosts/gone/health "Host: 127.0.0.1" ||
    fail "$name/gone" "GET /api/hosts/gone/health did not answer"
  expect_status "$name/gone" "$(answer "$name-gone")" 502
  expect_code "$name/gone" "$(answer "$name-gone")" upstream_unreachable
  body="$(cat "$WORK/responses/$name-gone.body")"
  for needle in "gone could not be reached at" "http://127.0.0.1:$gone_port"; do
    case "$body" in
      *"$needle"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/gone/sentence" "the refusal names neither the machine nor its origin: $body" ;;
    esac
  done
  # The same sentence for the list the card would have drawn: one refusal, two reads the page makes.
  fetch "$name-gone-list" GET /api/hosts/gone/sessions "Host: 127.0.0.1" ||
    fail "$name/gone_list" "GET /api/hosts/gone/sessions did not answer"
  expect_status "$name/gone_list" "$(answer "$name-gone-list")" 502
  expect_code "$name/gone_list" "$(answer "$name-gone-list")" upstream_unreachable

  # ---- 6. the registration's refusals, and the sentence the form prints ---------------------------
  # The name this machine's own sessions are reached by is reserved, so a row may not take it (ADR-0073).
  fetch_body "$name-bad-name" POST /api/hosts '{"name":"local","url":"http://127.0.0.1:9000"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" || fail "$name/bad_name" "POST /api/hosts did not answer"
  expect_status "$name/bad_name" "$(answer "$name-bad-name")" 422
  expect_code "$name/bad_name" "$(answer "$name-bad-name")" bad_host_name
  # The message is the sentence the form shows verbatim: a code in front of it would be a word to act on that is
  # not one (`hostFailure`). It names what to change.
  body="$(cat "$WORK/responses/$name-bad-name.body")"
  # The sentence as the body really spells it: the reserved word in quotes, escaped by JSON. A bare `local` would also
  # be satisfied by the word appearing in prose, which is the mistake `case_changes_screen` warns about.
  case "$body" in
    *'must not be \"local\"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/bad_name/sentence" "the refusal does not name the reserved word: $body" ;;
  esac
  # A port below the allowed range on a host the rule accepts: the URL's *port* arm, which the CLI must
  # state with the same sentence. (A loopback URL would be refused for its host instead, which is the arm below.)
  fetch_body "$name-bad-url" POST /api/hosts '{"name":"spare","url":"https://box.tail1a2b.ts.net:80"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" || fail "$name/bad_url" "POST /api/hosts did not answer"
  expect_status "$name/bad_url" "$(answer "$name-bad-url")" 422
  expect_code "$name/bad_url" "$(answer "$name-bad-url")" bad_host_url

  # ---- 7. adding and removing a row ---------------------------------------------------------------
  # A machine that is not there is **registered**, not refused: the row is stored `unreachable` with the sentence the
  # probe gave (item 16's "not removed"), which is the outcome the Add form redraws from. The URL is a MagicDNS shape
  # that resolves nowhere — the one a `POST` may name and a machine here cannot answer, since the API refuses loopback
  # where the seed does not (ADR-0073 D9, and `case_hosts` measures the same arm).
  fetch_body "$name-add" POST /api/hosts '{"name":"spare","url":"https://spare.tail1a2b.ts.net"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" || fail "$name/add" "POST /api/hosts did not answer"
  expect_status "$name/add" "$(answer "$name-add")" 201
  body="$(cat "$WORK/responses/$name-add.body")"
  case "$body" in
    *'"name":"spare"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/add/row" "the registered row is not the one asked for: $body" ;;
  esac
  case "$body" in
    *'"reach":"unreachable"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/add/reach" "a machine that answered nothing must be stored unreachable: $body" ;;
  esac
  fetch "$name-added" GET /api/hosts "Host: 127.0.0.1" || fail "$name/added" "GET /api/hosts did not answer"
  case "$(cat "$WORK/responses/$name-added.body")" in
    *'"name":"spare"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/added" "the row the form just added is not in the list" ;;
  esac
  # Removing it is a request to **this** machine's registry, never a forwarded one (ADR-0074).
  fetch "$name-remove" DELETE /api/hosts/spare \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" || fail "$name/remove" "DELETE /api/hosts/spare did not answer"
  expect_status "$name/remove" "$(answer "$name-remove")" 204
  fetch "$name-removed" GET /api/hosts "Host: 127.0.0.1" || fail "$name/removed" "GET /api/hosts did not answer"
  body="$(cat "$WORK/responses/$name-removed.body")"
  case "$body" in
    *'"name":"spare"'*) fail "$name/removed" "the removed row is still in the list: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  # A removal is one row's, not the list's: both the machines that were there before are still there.
  for needle in '"name":"workshop"' '"name":"gone"'; do
    case "$body" in
      *"$needle"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/removed/sibling" "removing one row lost $needle: $body" ;;
    esac
  done

  # ---- 8. a machine that forwards nothing cannot read another's rows -------------------------------
  # The peer is a `server` and its own registry names the hub, so the page on that machine would draw a group for the
  # hub — except that the forwarding route is registered only where a machine forwards (ADR-0074), so the read is the
  # peer's `404 not_found`, a sentence about an endpoint rather than about a machine. That is the fact behind
  # `forwards(role)`'s guard (ADR-0076 D3), and it is measured here rather than assumed.
  forward_fetch "$peer_port" "$name-peer-hosts" GET /api/hosts "Host: 127.0.0.1" ||
    fail "$name/peer_hosts" "GET /api/hosts on the peer did not answer"
  expect_status "$name/peer_hosts" "$(answer "$name-peer-hosts")" 200
  body="$(cat "$WORK/responses/$name-peer-hosts.body")"
  case "$body" in
    *'"name":"hub"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/peer_hosts/row" "the server's registry lost the row its seed names: $body" ;;
  esac
  forward_fetch "$peer_port" "$name-peer-forward" GET /api/hosts/hub/sessions "Host: 127.0.0.1" ||
    fail "$name/peer_forward" "GET /api/hosts/hub/sessions on the peer did not answer"
  expect_status "$name/peer_forward" "$(answer "$name-peer-forward")" 404
  expect_code "$name/peer_forward" "$(answer "$name-peer-forward")" not_found

  # ---- 9. the screen: this tree's shell, and the bundle's own words -------------------------------
  fetch "$name" GET /computers "Host: 127.0.0.1" || fail "$name/no_answer" "GET /computers did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  [ -f web/dist/index.html ] ||
    fail "$name/index_html" "web/dist/index.html is missing: run scripts/build.sh"
  # The `cmp` comes first on purpose: a grep alone would pass over a stale `dist/` that happened to carry the
  # strings (ADR-0067).
  cmp -s "$WORK/responses/$name.body" web/dist/index.html ||
    fail "$name/index_html" "/computers is not this tree's web/dist/index.html"
  assertions=$((assertions + 1))

  asset="$(sed -n 's/.*src="\([^"]*\.js\)".*/\1/p' web/dist/index.html | head -1)"
  [ -n "$asset" ] || fail "$name/asset" "the shell names no script to fetch"
  fetch "${name}_asset" GET "$asset" "Host: 127.0.0.1" || fail "$name/asset" "GET $asset did not answer"
  response="$(answer "${name}_asset")"
  expect_status "$name/asset" "$response" 200
  cmp -s "$WORK/responses/${name}_asset.body" "web/dist$asset" ||
    fail "$name/asset/bytes" "the served bundle is not this tree's web/dist$asset"
  assertions=$((assertions + 1))
  # One id per element the screen is made of — the assignment's own quotes, so deleting a container cannot leave a
  # sibling's id matching — plus the classes the grouping and the card are built from, plus the **seven** sentences a
  # decision already fixed: the two reach words (as old as the check, D1/D3), the skew (D5), the forced retry's label
  # and the Remove question (D6), the page's own machine (D3), the intro, and what a machine that forwards nothing can
  # read (D3). Every needle is counted, so one that found nothing cannot pass as one that did.
  for needle in \
    'id="computers"' 'id="computer-list"' 'id="computers-add"' 'id="computers-name"' \
    'id="computers-url"' 'id="computers-submit"' 'id="computers-error"' \
    'machine-group' 'computer-counts' 'computer-sentence' \
    'reachable at the last check' 'unreachable at the last check' \
    'a call one of them does not know answers 404' 'Remove anyway' 'local · this machine' \
    'Every machine this one may send a request to. A session lives on exactly one of them.' \
    'forwards nothing, so it can read only its own sessions' \
    'Its row is forgotten here; the machine itself is not changed.'; do
    grep -qF -- "$needle" "$WORK/responses/${name}_asset.body" ||
      fail "$name/asset/screen" "the served bundle does not carry $needle"
    assertions=$((assertions + 1))
  done
  # The page's path rule: the hub's forwarding prefix, spelled once for every request that crosses a machine.
  grep -qF -- '/api/hosts/' "$WORK/responses/${name}_asset.body" ||
    fail "$name/asset/api" "the served bundle never spells a forwarded path, so no request can cross machines"
  assertions=$((assertions + 1))

  # What this case made of the machine, it takes back, and the shared boot is put back for every case after it.
  stop_serve
  stop_peer
  rm -rf "$root"
  start_serve
}

# machine-to-machine trust (spec item 4, V11). A machine takes forwarded calls from its hub and from nobody
# else, and the two facts that decide it are measured here against a real listener: the whois of the **address a
# connection came from** (never a header the caller writes) and the `allowed_hubs` list that answer is compared
# against. The three arms are the story's own: an empty list verifies nobody (the documented ACLs-only fallback), a
# listed node is served, and another node — or an identity that cannot be read — is refused in each listener's own
# words with one logged reason.
#
# Two loopback boots cannot produce a tailnet peer, so this case stands two things in:
#
# - a `tailscale` on `PATH` (the shape `deploy_machine_shims` already uses: the paths arrive through the
#   environment, so the script itself is plain `sh`) that records every argv and answers `whois --json <ip>` from a
#   state file — which is what lets one listener be asked about a listed node, another node and a daemon that is not
#   there without a third boot;
# - a client **bound to 127.0.0.2**, the only way to be an address other than 127.0.0.1 without root — and the
#   distinction is the rule's own, not a trick: `tailscale serve` connects from 127.0.0.1 (deploy step 6, the front
#   door), and the rest of `127/8` is a peer like any other.
#
# It needs no daemon and no image, and it leaves nothing behind: its own root, its own boot, and the shared boot
# put back. `tests/trust.rs` drives the same arms in-process and the shim's three answers as values.
case_machine_trust() {
  local name=case_machine_trust root="$repo_root/target/verify-trust-$$"
  local peer_root="$root/peer"
  local config="$peer_root/config.toml" data="$peer_root/data"
  local bin="$root/bin" answer="$root/whois.json" failure="$root/whois.err" log="$root/whois.log"
  local peer_control peer_preview saved_path body head
  local id=0123456789abcdef
  local have_python3=1
  command -v python3 >/dev/null 2>&1 || have_python3=0
  if [ "$have_python3" -eq 0 ]; then
    printf 'SKIP %s: no python3 to bind a client to 127.0.0.2, which is the only way to be a peer on one box (tests/trust.rs drives every arm)\n' "$name"
    return 0
  fi

  stop_serve
  rm -rf "$root"
  mkdir -p "$peer_root/projects/proj" "$data" "$bin" || fail "$name/mkdir" "could not create $root"
  peer_control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  peer_preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"

  # The gate's tailscale: record the call, then answer from the state file, or fail in the words this machine's real
  # one prints. `tests/trust.rs` owns the same three answers as a decision table.
  export HS_TRUST_LOG="$log" HS_TRUST_ANSWER="$answer" HS_TRUST_FAILURE="$failure"
  cat >"$bin/tailscale" <<'SHIM'
#!/bin/sh
# The gate's tailscale: no daemon is asked, and every argv is one line in the log.
set -eu
printf '%s\n' "$*" >>"$HS_TRUST_LOG"
if [ -f "$HS_TRUST_ANSWER" ]; then
  cat "$HS_TRUST_ANSWER"
  exit 0
fi
if [ -f "$HS_TRUST_FAILURE" ]; then
  cat "$HS_TRUST_FAILURE" >&2
fi
exit 1
SHIM
  chmod 0755 "$bin/tailscale"
  printf 'failed to connect to local tailscaled\n' >"$failure"
  saved_path="$PATH"
  PATH="$bin:$PATH"

  # The peer machine's config, rewritten between the two boots: the list is read at boot, and the two arms this case
  # needs are "no list" and "a list".
  write_trust_config() {
    local hubs="$1"
    cat >"$config" <<EOF
role = "server"
backend = "stub"
control_origin = "http://127.0.0.1:$peer_control"
preview_listen = "127.0.0.1:$peer_preview"
preview_origin = "http://127.0.0.1:$peer_preview"
allowed_hubs = $hubs
EOF
  }

  # as_peer <label> <source> <port> <path> [Header: value]: one request from a chosen **source address**, because a
  # client that cannot choose one is always the front door. The answer is saved under the two names the harness's own
  # assertions read (`.http` the whole answer, `.body` after the header block), so `answer`/`expect_status`/
  # `expect_code` work unchanged.
  as_peer() {
    local label="$1" source="$2" then_port="$3" path="$4" extra="${5:-}"
    timeout 20 python3 - "$source" "$then_port" "$path" "$extra" "$WORK/responses/$label" <<'PYCLIENT' ||
import socket, sys

source, port, path, extra, out = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4], sys.argv[5]
connection = socket.socket()
connection.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
connection.settimeout(10)
connection.bind((source, 0))
connection.connect(("127.0.0.1", port))
head = "GET %s HTTP/1.1\r\nHost: 127.0.0.1\r\n" % path
if extra:
    head += extra + "\r\n"
head += "Connection: close\r\n\r\n"
connection.sendall(head.encode())
raw = b""
while True:
    chunk = connection.recv(65536)
    if not chunk:
        break
    raw += chunk
connection.close()
head, _, body = raw.partition(b"\r\n\r\n")
open(out + ".http", "wb").write(raw)
open(out + ".body", "wb").write(body)
print(head.split(b"\r\n")[0].decode())
PYCLIENT
      fail "$name/$label" "the request from $source:$then_port$path did not answer"
  }

  # ---- 1. no list: the ACLs-only fallback, measured -----------------------------------------------
  # V11's own "If it fails" cell: a machine that lists no hubs verifies nobody, and tailnet ACLs are the control. It
  # is the default config, so this arm is also the one that keeps every existing deployment working.
  write_trust_config '[]'
  start_peer "$config" "$data" "$peer_control"
  as_peer "$name-open" 127.0.0.2 "$peer_port" /api/sessions
  expect_status "$name/open" "$(answer "$name-open")" 200
  if [ -f "$log" ]; then
    fail "$name/open/whois" "an empty allowed_hubs asked a whois, so it verified something: $(cat "$log")"
  fi
  assertions=$((assertions + 1))
  stop_peer

  # ---- 2. a list, and the identity comes from the address -----------------------------------------
  write_trust_config '["workshop"]'
  start_peer "$config" "$data" "$peer_control"
  printf '{"Node":{"Name":"workshop.tail1a2b.ts.net"}}\n' >"$answer"
  as_peer "$name-listed" 127.0.0.2 "$peer_port" /api/sessions
  expect_status "$name/listed" "$(answer "$name-listed")" 200
  # The argv is the measurement that it is the **address** and nothing the caller wrote: no header ever reaches this
  # child, and `request_head` does not send an identity header at all.
  [ -f "$log" ] || fail "$name/listed/whois" "a listed peer was admitted without a whois"
  grep -qx 'whois --json 127.0.0.2' "$log" ||
    fail "$name/listed/whois" "the whois was not asked about the connection's address: $(cat "$log")"
  assertions=$((assertions + 1))

  # ---- 3. the front door costs no child -----------------------------------------------------------
  # `tailscale serve` proxies both origins to 127.0.0.1, so every browser call and every local tool arrives from
  # there. A machine that whois-ed that address would refuse every browser it has: 127.0.0.1 is no tailnet node.
  : >"$log"
  forward_fetch "$peer_port" "$name-front" GET /api/sessions "Host: 127.0.0.1" ||
    fail "$name/front" "the front door's request did not answer"
  expect_status "$name/front" "$(answer "$name-front")" 200
  if [ -s "$log" ]; then
    fail "$name/front/whois" "the front door asked a whois: $(cat "$log")"
  fi
  assertions=$((assertions + 1))

  # ---- 4. another tailnet node is refused, in the envelope, once in the log ------------------------
  printf '{"Node":{"Name":"stranger.tail1a2b.ts.net"}}\n' >"$answer"
  as_peer "$name-stranger" 127.0.0.2 "$peer_port" /api/sessions
  expect_status "$name/stranger" "$(answer "$name-stranger")" 403
  expect_code "$name/stranger" "$(answer "$name-stranger")" peer_not_allowed
  body="$(cat "$WORK/responses/$name-stranger.body")"
  case "$body" in
    *127.0.0.2*) assertions=$((assertions + 1)) ;;
    *) fail "$name/stranger/peer" "the refusal names no peer address: $body" ;;
  esac
  # Acceptance bullet 5: never the configuration. The sentence's own "hub" is the domain word, so the entry this
  # machine lists is the needle.
  case "$body" in
    *workshop*) fail "$name/stranger/leak" "the refusal names a configured hub: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  grep -q 'a call from a peer this machine does not trust was refused' "$WORK/serve-peer.err" ||
    fail "$name/stranger/log" "the refusal was not logged: $(cat "$WORK/serve-peer.err")"
  grep -q 'peer=127.0.0.2' "$WORK/serve-peer.err" ||
    fail "$name/stranger/log-peer" "the log line names no peer address: $(cat "$WORK/serve-peer.err")"
  check "$name/stranger/log-reason" 1 \
    "$(grep -c 'reason=peer_not_allowed' "$WORK/serve-peer.err" || true)"
  check "$name/stranger/log-once" 1 \
    "$(grep -c 'a call from a peer this machine does not trust was refused' "$WORK/serve-peer.err" || true)"

  # ---- 5. a header the caller supplies changes nothing --------------------------------------------
  # Item 4's "not by a header the caller supplies": the one header `tailscale serve` would add is a claim, and the
  # address is the fact. The forged value is the **listed** name, which is what makes this arm able to fail — an
  # implementation that honoured the header would answer 200, and a name nobody lists would pass either way.
  as_peer "$name-forged" 127.0.0.2 "$peer_port" /api/sessions "Tailscale-User-Login: workshop"
  expect_status "$name/forged" "$(answer "$name-forged")" 403
  expect_code "$name/forged" "$(answer "$name-forged")" peer_not_allowed

  # ---- 6. an identity that cannot be read is its own code -----------------------------------------
  # The arm this machine can really run: `tailscaled` is not answering here, and the sentence must not be the
  # daemon's English.
  rm -f "$answer"
  as_peer "$name-unverifiable" 127.0.0.2 "$peer_port" /api/sessions
  expect_status "$name/unverifiable" "$(answer "$name-unverifiable")" 403
  expect_code "$name/unverifiable" "$(answer "$name-unverifiable")" peer_unverifiable
  body="$(cat "$WORK/responses/$name-unverifiable.body")"
  case "$body" in
    *tailscaled*) fail "$name/unverifiable/prose" "the answer repeats the daemon's English: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  grep -q 'reason=peer_unverifiable' "$WORK/serve-peer.err" ||
    fail "$name/unverifiable/log" "the failure was not logged: $(cat "$WORK/serve-peer.err")"
  assertions=$((assertions + 1))

  # ---- 7. the preview hop is gated by the same decision, in this listener's own words ---------------
  # Two hops reach a peer (ADR-0074 D1), and a check on only one of them leaves the other open: the preview listener
  # answers a **sentence** in plain text, and the accept arm reaches its own "no such session" — which is how "the
  # machine check passed and the listener carried on" is measured rather than claimed.
  printf '{"Node":{"Name":"workshop.tail1a2b.ts.net"}}\n' >"$answer"
  as_peer "$name-preview-listed" 127.0.0.2 "$peer_preview_port" "/local/$id/"
  expect_status "$name/preview_listed" "$(answer "$name-preview-listed")" 404
  body="$(cat "$WORK/responses/$name-preview-listed.body")"
  case "$body" in
    *"no session is called $id"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/preview_listed" "a listed peer did not reach the listener's own answer: $body" ;;
  esac

  printf '{"Node":{"Name":"stranger.tail1a2b.ts.net"}}\n' >"$answer"
  as_peer "$name-preview-stranger" 127.0.0.2 "$peer_preview_port" "/local/$id/"
  expect_status "$name/preview_stranger" "$(answer "$name-preview-stranger")" 403
  head="$(sed -n '1,/^\r$/p' "$WORK/responses/$name-preview-stranger.http")"
  case "$head" in
    *"text/plain"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/preview_stranger/plain" "a preview refusal must be plain text, not an envelope: $head" ;;
  esac
  body="$(cat "$WORK/responses/$name-preview-stranger.body")"
  case "$body" in
    *127.0.0.2*) assertions=$((assertions + 1)) ;;
    *) fail "$name/preview_stranger/peer" "the preview refusal names no peer address: $body" ;;
  esac
  case "$body" in
    *workshop*) fail "$name/preview_stranger/leak" "the preview refusal names a configured hub: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  case "$body" in
    *'"code"'*) fail "$name/preview_stranger/envelope" "this listener has no envelope: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac

  # What this case made of the machine, it takes back — including the `PATH` the shim was on, so the shared boot
  # after it runs the machine's own `tailscale` and not the gate's.
  stop_peer
  PATH="$saved_path"
  unset HS_TRUST_LOG HS_TRUST_ANSWER HS_TRUST_FAILURE
  rm -rf "$root"
  start_serve
}



# the idle stop. A session left alone stops costing money, and it is the artifact's *own*
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

# run_drain <label> <admin dir> [flags...]: the artifact's own `drain`, its output under $WORK, and its exit
# status on stdout so a caller can `check` it.
#
# The command is the client under test — the acceptance is about what an operator types, not about the
# protocol — so the gate never hand-writes a drain line to the socket. The socket's own protocol is
# `tests/drain.rs`'s, where a malformed line can be sent without a shell.
run_drain() {
  local label="$1" admin="$2"
  shift 2
  local status=0
  "$artifact" drain --admin-dir "$admin" "$@" >"$WORK/$label.out" 2>"$WORK/$label.err" || status=$?
  printf '%s\n' "$status"
}

# drain_row <database> <id>: `<status> <reason> <expected_stop>` for one row, which is the two facts no client
# is sent. `mode=ro` like `usage_of`, and `none` for a row that is gone.
drain_row() {
  python3 - "$1" "$2" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
row = connection.execute(
    "select status, status_reason, expected_stop from sessions where id = ?", (sys.argv[2],)
).fetchone()
print("none" if row is None else " ".join(str(field) for field in row))
PY
}

# the drain, over the machine's admin socket and only there.
#
# It owns its boot, for two reasons: the drain stops **every** idle session, which would take `$created_id` with
# it, and the shared config's cap of one live session cannot hold the two this case needs (one to turn, one to
# leave idle). It needs `python3` for the row facts the API does not carry — `status_reason` is on the row and
# `expected_stop` is a column no client is sent (ADR-0038) — and prints one SKIP line without it.
#
# What it can measure on a machine with a stub turn of ~2 s (ten 200 ms frames):
#
# 1. the socket exists at `<admin dir>/drain.sock` and the admin directory is the run's (`--admin-dir`);
# 2. `POST /api/drain` is the fallback's 404 — the API table's "not reachable over HTTP" — and a raw drain line
#    written to the **control port** is answered as a malformed HTTP request, not as a drain;
# 3. a `--wait` shorter than the turn exits 1 and says the drain was left on;
# 4. in that window the create and the prompt answer `503 draining` while both reads answer 200;
# 5. a `--wait` longer than the turn waits for it and then stops the idle sessions with `server_restart`, as a
#    transition in the log, a row fact and one log line;
# 6. a second drain says `already draining` and finds nothing left to stop;
# 7. a resume is refused while draining and a delete still works.
case_drain() {
  local name=case_drain response body first second status stopped_line admin
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'
  admin="$WORK/data/run"

  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to read the rows back (tests/drain.rs covers the rows)\n' "$name"
    return 0
  fi
  if [ -n "$serve_pid" ] && kill -0 "$serve_pid" 2>/dev/null; then
    stop_serve
  fi
  # Three live slots and no `idle_stop_min`: this case watches two sessions of its own and no second policy may
  # take one of them. The preview listener asks the kernel for a port like every other boot (start_serve reads
  # it, and a boot without it fails this case for a reason that is not its subject).
  printf 'owner = "%s"\nmax_live_sessions = 3\nbackend = "stub"\npreview_listen = "127.0.0.1:0"\n' "$owner" \
    >"$WORK/config-drain.toml"
  start_serve "$WORK/config-drain.toml" "$WORK/data-drain"

  # 1. The socket. `-S` is the kernel's own answer to "is this a socket", which is the fact step 2's directory
  #    exists for.
  if [ -S "$admin/drain.sock" ]; then
    assertions=$((assertions + 1))
  else
    fail "$name/socket" "no admin socket at $admin/drain.sock: $(find "$WORK" -name 'drain.sock' 2>/dev/null)"
  fi

  # 2a. Not a route. The guards pass, so the answer is the fallback's own envelope rather than a refusal.
  fetch_body "${name}_route" POST /api/drain '' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_route/no_answer" "POST /api/drain did not answer"
  response="$(answer "${name}_route")"
  expect_status "${name}_route" "$response" 404
  expect_code "${name}_route" "$response" not_found

  # 2b. A drain line written to the **control** port is not a drain: the listener speaks HTTP and answers this
  #     as a malformed request. The answer's status is the measurement — a 400 is a refusal by the HTTP parser,
  #     and nothing about the drain happened.
  if printf 'drain 600000\r\n' | timeout 10 python3 -c '
import socket, sys
connection = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=10)
connection.sendall(sys.stdin.buffer.read())
answer = b""
while b"\r\n" not in answer:
    chunk = connection.recv(4096)
    if not chunk:
        break
    answer += chunk
print(answer.split(b"\r\n")[0].decode(errors="replace"))
' "$port" >"$WORK/responses/$name.control" 2>&1; then
    case "$(cat "$WORK/responses/$name.control")" in
      *" 400 "*) assertions=$((assertions + 1)) ;;
      *)
        fail "$name/control_port" \
          "the control listener did not refuse a drain line as a malformed request: $(cat "$WORK/responses/$name.control")"
        ;;
    esac
  else
    fail "$name/control_port" "the raw drain line on the control port could not be asked"
  fi

  # 3. Two sessions: `first` is left idle, `second` is turned so the drain has something to wait for.
  fetch_body "${name}_create" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-drain-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  first="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$first" ] || fail "${name}_create/id" "the response names no id: $body"
  wait_for_idle_id "${name}_idle" "$first"

  fetch_body "${name}_create2" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-drain-2" ||
    fail "${name}_create2/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create2" "$(answer "${name}_create2")" 202
  body="$(cat "$WORK/responses/${name}_create2.body")"
  second="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$second" ] || fail "${name}_create2/id" "the response names no id: $body"
  wait_for_idle_id "${name}_create2_idle" "$second"

  fetch_body "${name}_prompt" POST "/api/sessions/$second/prompt" '{"prompt":"read the app shell"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt/no_answer" "POST /api/sessions/$second/prompt did not answer"
  expect_status "${name}_prompt" "$(answer "${name}_prompt")" 202

  # 4. A wait shorter than the stub's turn: the command must be honest about it. `--wait 200ms` is under one
  #    frame of the turn, so the deadline cannot be met by luck on any machine that runs this case at all.
  status="$(run_drain "${name}_expired" "$admin" --wait 200ms)"
  check "${name}_expired/exit" 1 "$status"
  grep -qx 'drain: state: draining' "$WORK/${name}_expired.out" ||
    fail "${name}_expired/state" "the drain did not report its state: $(cat "$WORK/${name}_expired.out")"
  grep -q '^drain: waiting: 1 turn still running$' "$WORK/${name}_expired.out" ||
    fail "${name}_expired/waiting" "the drain did not report what it was waiting for: $(cat "$WORK/${name}_expired.out")"
  grep -qx 'drain: result: expired' "$WORK/${name}_expired.out" ||
    fail "${name}_expired/result" "the wait's expiry is not the result: $(cat "$WORK/${name}_expired.out")"
  grep -q 'still draining' "$WORK/${name}_expired.out" ||
    fail "${name}_expired/left_on" "the report does not say the drain was left on: $(cat "$WORK/${name}_expired.out")"

  # 5. In that window: the two requests the spec names are refused, with the envelope and the new code, and the
  #    reads still answer. This is the acceptance's second bullet, measured in one moment rather than hoped for.
  fetch_body "${name}_refused_create" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-drain-3" ||
    fail "${name}_refused_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_refused_create")"
  expect_status "${name}_refused_create" "$response" 503
  expect_code "${name}_refused_create" "$response" draining
  expect_header "${name}_refused_create" "$response" content-type "application/json"
  grep -q 'holosmith drain --wait' "$WORK/responses/${name}_refused_create.body" ||
    fail "${name}_refused_create/sentence" \
      "the refusal does not name the drain and its wait: $(cat "$WORK/responses/${name}_refused_create.body")"

  fetch_body "${name}_refused_prompt" POST "/api/sessions/$first/prompt" '{"prompt":"hello"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_refused_prompt/no_answer" "POST /api/sessions/$first/prompt did not answer"
  response="$(answer "${name}_refused_prompt")"
  expect_status "${name}_refused_prompt" "$response" 503
  expect_code "${name}_refused_prompt" "$response" draining

  fetch "${name}_read_list" GET /api/sessions "Host: 127.0.0.1" ||
    fail "${name}_read_list/no_answer" "GET /api/sessions did not answer"
  expect_status "${name}_read_list" "$(answer "${name}_read_list")" 200
  fetch "${name}_read_one" GET "/api/sessions/$second" "Host: 127.0.0.1" ||
    fail "${name}_read_one/no_answer" "GET /api/sessions/$second did not answer"
  response="$(answer "${name}_read_one")"
  expect_status "${name}_read_one" "$response" 200
  case "$(cat "$WORK/responses/${name}_read_one.body")" in
    *'"status":"running"'*) assertions=$((assertions + 1)) ;;
    *)
      fail "${name}_read_one/running" \
        "the read did not answer the live row while draining: $(cat "$WORK/responses/${name}_read_one.body")"
      ;;
  esac

  # 6. A drain that waits: the turn finishes (the stub settles `running -> idle`), and the idle sessions are
  #    stopped after it — the acceptance's third bullet, which is a sequence and not two independent facts.
  status="$(run_drain "${name}_drained" "$admin" --wait 30s)"
  check "${name}_drained/exit" 0 "$status"
  grep -qx 'drain: result: drained' "$WORK/${name}_drained.out" ||
    fail "${name}_drained/result" "the drain did not finish: $(cat "$WORK/${name}_drained.out")"
  stopped_line="$(sed -n 's/^drain: stopped: \(.*\)$/\1/p' "$WORK/${name}_drained.out")"
  check "${name}_drained/stopped" "2 sessions with reason server_restart" "$stopped_line"

  #    The rows. The container's stop is a task of its own (ADR-0024's shape), so `expected_stop`
  #    is polled rather than read once: a `stopped` row is not a promise that the spawned stop has run.
  local session row
  for session in "$first" "$second"; do
    row="none"
    for _ in $(seq 1 50); do
      row="$(drain_row "$WORK/data-drain/app.db" "$session")"
      [ "$row" = "stopped server_restart 1" ] && break
      sleep 0.1
    done
    check "${name}_row_${session}" "stopped server_restart 1" "$row"
  done

  #    And the transition reached the transcript, which is the client's copy of why a row stopped.
  fetch_sse_until "${name}_stream" "/api/sessions/$first/events" '"reason":"server_restart"' \
    "Host: 127.0.0.1" ||
    fail "${name}_stream/no_answer" "GET /api/sessions/$first/events did not answer"
  expect_status "${name}_stream" "$(answer "${name}_stream")" 200
  grep -q '"from":"idle","to":"stopped","step":null,"reason":"server_restart"' \
    "$WORK/responses/${name}_stream.body" ||
    fail "${name}_stream/transition" \
      "the drain's transition is not in the session's transcript: $(cat "$WORK/responses/${name}_stream.body")"

  #    One line per stop, for an operator reading journald.
  grep -q "holosmith::drain: the session was stopped for a restart session=$first" "$serve_err" ||
    fail "${name}/log" \
      "the drain wrote no line for $first: $(grep 'holosmith::drain' "$serve_err" | sed -n '1,5p')"

  # 7. A second drain joins rather than starting again, and finds nothing left to stop.
  status="$(run_drain "${name}_again" "$admin" --wait 5s)"
  check "${name}_again/exit" 0 "$status"
  grep -qx 'drain: state: already draining' "$WORK/${name}_again.out" ||
    fail "${name}_again/state" "a second drain did not say the machine was already draining: $(cat "$WORK/${name}_again.out")"
  grep -qx 'drain: stopped: 0 sessions with reason server_restart' "$WORK/${name}_again.out" ||
    fail "${name}_again/stopped" "the second drain claims to have stopped something: $(cat "$WORK/${name}_again.out")"
  grep -qx 'drain: result: drained' "$WORK/${name}_again.out" ||
    fail "${name}_again/result" "the second drain did not finish: $(cat "$WORK/${name}_again.out")"

  # 8. What stays open while draining: a resume provisions a container and is refused, a delete removes a
  #    session and is not.
  fetch_body "${name}_resume" POST "/api/sessions/$first/resume" '' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_resume/no_answer" "POST /api/sessions/$first/resume did not answer"
  response="$(answer "${name}_resume")"
  expect_status "${name}_resume" "$response" 503
  expect_code "${name}_resume" "$response" draining

  fetch_body "${name}_delete" DELETE "/api/sessions/$second" '' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete/no_answer" "DELETE /api/sessions/$second did not answer"
  expect_status "${name}_delete" "$(answer "${name}_delete")" 202

  # The shared boot comes back: the cases after this one talk to `$port` and to `$created_id`.
  stop_serve
  start_serve
}

# the fifth acceptance bullet: a session that was **live** at drain time is `idle` again on the next
# startup.
#
# A completed drain leaves nothing live — it waits for the turns and stops the idle sessions — so the only state
# that exercises this bullet is the drain that could not finish: an expired wait leaves the row `running`, and the
# `kill -9` then leaves it to the startup reconcile (ADR-0053), which is the mechanism item 10 step 6 names. The
# other half is that the new process is **not** draining: the flag is the process's, and a restart is exactly what
# it exists to precede.
case_drain_restart() {
  local name=case_drain_restart response body first status admin
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'
  admin="$WORK/data/run"

  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to read the crashed row back (tests/drain.rs covers the sequence)\n' "$name"
    return 0
  fi
  if [ -n "$serve_pid" ] && kill -0 "$serve_pid" 2>/dev/null; then
    stop_serve
  fi
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "stub"\npreview_listen = "127.0.0.1:0"\n' "$owner" \
    >"$WORK/config-drain-restart.toml"
  start_serve "$WORK/config-drain-restart.toml" "$WORK/data-drain-restart"

  fetch_body "${name}_create" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-drain-restart-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  first="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$first" ] || fail "${name}_create/id" "the response names no id: $body"
  wait_for_idle_id "${name}_idle" "$first"

  fetch_body "${name}_prompt" POST "/api/sessions/$first/prompt" '{"prompt":"read the app shell"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt/no_answer" "POST /api/sessions/$first/prompt did not answer"
  expect_status "${name}_prompt" "$(answer "${name}_prompt")" 202

  # The drain that cannot finish, and the bullet's other half: it exits 1, and the state it was asked for is on.
  status="$(run_drain "${name}_expired" "$admin" --wait 200ms)"
  check "${name}_expired/exit" 1 "$status"
  grep -qx 'drain: result: expired' "$WORK/${name}_expired.out" ||
    fail "${name}_expired/result" "the wait's expiry is not the result: $(cat "$WORK/${name}_expired.out")"
  fetch_body "${name}_still" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-drain-restart-2" ||
    fail "${name}_still/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_still")"
  expect_status "${name}_still" "$response" 503
  expect_code "${name}_still" "$response" draining

  # **The crash**, while the row is `running` and the drain is on: `kill -9`, the story's own word, so nothing
  # shuts anything down gracefully and the reconcile is the only thing left that can bring the row back.
  kill -9 "$serve_pid" || fail "${name}/kill" "could not kill the server (pid $serve_pid)"
  wait "$serve_pid" 2>/dev/null || true
  serve_pid=""
  start_serve "$WORK/config-drain-restart.toml" "$WORK/data-drain-restart"

  # Bullet 5: the row the crash left `running` is `idle` again, with the reconcile's own reason.
  wait_for_idle_id "${name}_recovered" "$first"
  case "$(cat "$WORK/responses/${name}_recovered.body")" in
    *'"status":"idle"'*'"status_reason":"interrupted"'*) assertions=$((assertions + 1)) ;;
    *)
      fail "${name}_recovered/reason" \
        "the row came back without the reconcile's reason: $(cat "$WORK/responses/${name}_recovered.body")"
      ;;
  esac

  # And the new process is not draining: the flag belonged to the process that had it. A create is accepted,
  # which is the only way a client can ask.
  fetch_body "${name}_accepted" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-drain-restart-3" ||
    fail "${name}_accepted/no_answer" "POST /api/sessions did not answer"
  expect_status "${name}_accepted" "$(answer "${name}_accepted")" 202
  body="$(cat "$WORK/responses/${name}_accepted.body")"
  local second
  second="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$second" ] || fail "${name}_accepted/id" "the response names no id: $body"

  remove_session "${name}_removed_first" "$first"
  remove_session "${name}_removed_second" "$second"
  stop_serve
  start_serve
}

# a delete for a session the table does not hold is ADR-0008's 404 `not_found` — the
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

# the first `DELETE` route in this crate passes the guards like every other request that
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

# at the cap, a create stops the least recently active idle session and takes its slot. The
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

# the proof from the shipped artifact: the stop the server performs records that it was its own, and
# a new container starts without the last one's flag.
#
# The artifact cannot show the story's other half — a container's *exit* — because this build has no process
# whose exit anybody observes: the container runs detached on `sleep infinity` until attaches pi's
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

# the proof from the artifact: a ceiling under one turn's spend ends the turn that spends it, and
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

# the proof from the shipped artifact: a session's spend is a column that survives the two things this
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
  # taken the instant the row says `idle` can be one frame short.
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

# the proof from the artifact: a message completed more than a day ago becomes one
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

# the two halves from the artifact: a client that opens the stream with no `Last-Event-ID` gets the
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

# the proof from the artifact: with `project_backup` on, the first session of a day bundles the
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

# the proof from the artifact: the worktree step is host git, so a create makes `.worktrees/<id>` on
# branch `agent/<id>` from the repository's HEAD, records both on the row, and — because the *next* step is
# the session's network, which runs `docker network create` against the socket this case answers
# with a `_ping` shim and nothing else — the provisioner takes the worktree back, leaving the registration
# clean. (Before the network step refused outright, naming its story in the reason; now it fails on a
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

  # A delete whose worktree is already gone is not a failure: `force` turns the snapshot refusal
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

# the proof from the artifact: the changes view's summary, over a real repository, with **no daemon**.
#
# Every acceptance bullet is a git fact, so the case owns the repository and moves it between two readings: a
# commit on `agent/<id>`, a commit on `main` (the target moving on), an uncommitted file (the worktree's
# `dirty`), then a planted conflict for `mergeable: false`, then a removed and a pruned worktree for the 409.
# The numbers are compared with `git rev-list --count` run here rather than with the code's own arithmetic.
#
# The boot is `case_worktree`'s: `backend = "docker"` against a socket that answers the preflight's `_ping`
# and nothing else, so the create's worktree step runs on the host and the network step then fails — which is
# the measurement for "no container is involved". The provisioner undoes what it made, so the case puts the
# worktree back with the step's own command; that is a fixture, and it is the command the step runs.
case_changes() {
  local name=case_changes response body id base tip wt ahead behind before
  # Two statements, not one: in a single `local` the right-hand sides expand before either assignment, so
  # `repo="$projects/holosmith"` on the same line reads an unset `projects` and `set -u` stops the run.
  local projects="$WORK/projects-changes"
  local repo="$projects/holosmith"
  # The guard comes first, before `stop_serve`, for the reason `case_worktree` gives: a skipped case that
  # stopped the server leaves the phase's own teardown with no pid to signal.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to answer the docker preflight (tests/changes.rs covers this)\n' "$name"
    return 0
  fi
  stop_serve
  mkdir -p "$repo"
  git -C "$repo" init --quiet -b main || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet --allow-empty \
    -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  base="$(git -C "$repo" rev-parse HEAD)" || fail "$name/git" "git rev-parse did not work in $repo"

  socket_pid="$(fake_docker_ping "$WORK/docker-changes.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker-changes.sock" ] && break
    sleep 0.05
  done
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-changes.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$WORK" >"$WORK/config-changes.toml"
  start_serve "$WORK/config-changes.toml" "$WORK/data-changes"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-changes-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"

  # The row is what the wait is on: the worktree step is a background task's, and the network step's failure
  # is what ends it — with the worktree and the branch taken back.
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
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_row/base" "the row does not record the branch's base commit: $body"
  assertions=$((assertions + 1))

  wt="$projects/.worktrees/$id"
  # The provisioner removed what its worktree step made (ADR-0029), so the case makes it again with the
  # step's own command — absolute paths, because `worktree add` resolves a relative one against the
  # repository.
  git -C "$repo" worktree add --quiet -b "agent/$id" "$wt" "$base" ||
    fail "${name}/worktree" "git worktree add did not work for $wt"
  printf 'one\n' >"$wt/a.txt" || fail "${name}/write" "the session's file is not writable"
  git -C "$wt" add a.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the session's change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  # A second commit on the branch, so the two counts below **differ** (2 and 1). With one commit each way a
  # swapped `behind` range would pass, and this case exists to measure which side of the range is which.
  printf 'two\n' >"$wt/c.txt" || fail "${name}/write" "the session's second file is not writable"
  git -C "$wt" add c.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the session's second change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"

  # The target moves on: one commit that is not on the branch, which is what `behind` counts.
  printf 'the target\n' >"$repo/t.txt" || fail "${name}/write" "the target's file is not writable"
  git -C "$repo" add t.txt || fail "${name}/add" "git add did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet -m "the target moves on" ||
    fail "${name}/commit" "git commit did not work in $repo"
  # An uncommitted file: the worktree holds work, which is `dirty` and nothing else.
  printf 'wip\n' >"$wt/untracked.txt" || fail "${name}/write" "the untracked file is not writable"

  fetch "${name}_summary" GET "/api/sessions/$id/changes" "Host: 127.0.0.1" ||
    fail "${name}_summary/no_answer" "GET /api/sessions/$id/changes did not answer"
  response="$(answer "${name}_summary")"
  expect_status "${name}_summary" "$response" 200
  expect_header "${name}_summary" "$response" "Content-Type" "application/json"
  body="$(cat "$WORK/responses/${name}_summary.body")"
  printf '%s\n' "$body" | grep -qF "\"branch\":\"agent/$id\"" ||
    fail "${name}_summary/branch" "the answer names no session branch: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_summary/base" "the answer's base commit is not the row's: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF "\"head\":\"$tip\"" ||
    fail "${name}_summary/head" "the answer's head is not the branch's tip: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"target":"main"' ||
    fail "${name}_summary/target" "the repository's own branch is not the target: $body"
  assertions=$((assertions + 1))
  # Both counts against git's own arithmetic: two commits on the branch, one on the target. The two must
  # differ, or the assertions below cannot tell `behind` from `ahead`.
  ahead="$(git -C "$repo" rev-list --count "$base..agent/$id")" ||
    fail "${name}/ahead" "git rev-list did not work"
  behind="$(git -C "$repo" rev-list --count "agent/$id..main")" ||
    fail "${name}/behind" "git rev-list did not work"
  check "${name}/counts_differ" "2 1" "$ahead $behind"
  check "${name}_summary/ahead" "\"ahead\":$ahead" \
    "$(printf '%s\n' "$body" | sed -n 's/.*\("ahead":[0-9]*\).*/\1/p')"
  check "${name}_summary/behind" "\"behind\":$behind" \
    "$(printf '%s\n' "$body" | sed -n 's/.*\("behind":[0-9]*\).*/\1/p')"
  printf '%s\n' "$body" | grep -qF '"dirty":true' ||
    fail "${name}_summary/dirty" "an uncommitted file did not make the worktree dirty: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"mergeable":true' ||
    fail "${name}_summary/mergeable" "a clean merge is not mergeable: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"path":"a.txt","status":"A","additions":1,"deletions":0' ||
    fail "${name}_summary/files" "the file list does not carry a.txt's line counts: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"touches_ci":false' ||
    fail "${name}_summary/touches_ci" "a plain file touched CI: $body"
  assertions=$((assertions + 1))

  # A planted conflict on the same path, and the two measurements the acceptance asks for: the merge is not
  # possible, and nothing on disk moved — no working tree was touched by `merge-tree --write-tree`.
  printf 'the target\x27s version\n' >"$repo/a.txt" || fail "${name}/write" "the conflicting file is not writable"
  git -C "$repo" add a.txt || fail "${name}/add" "git add did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet -m "the conflict" ||
    fail "${name}/commit" "git commit did not work in $repo"
  before="$(git -C "$repo" rev-parse HEAD) $(git -C "$repo" status --porcelain) | $(git -C "$wt" status --porcelain)" ||
    fail "${name}/status" "git status did not work"
  fetch "${name}_conflict" GET "/api/sessions/$id/changes" "Host: 127.0.0.1" ||
    fail "${name}_conflict/no_answer" "GET /api/sessions/$id/changes did not answer"
  expect_status "${name}_conflict" "$(answer "${name}_conflict")" 200
  body="$(cat "$WORK/responses/${name}_conflict.body")"
  printf '%s\n' "$body" | grep -qF '"mergeable":false' ||
    fail "${name}_conflict/mergeable" "a conflict is reported as mergeable: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF 'a.txt' ||
    fail "${name}_conflict/reason" "the conflict names no path: $body"
  assertions=$((assertions + 1))
  check "${name}_conflict/unmoved" "$before" \
    "$(git -C "$repo" rev-parse HEAD) $(git -C "$repo" status --porcelain) | $(git -C "$wt" status --porcelain)"

  # A missing worktree is the story's 409 with a sentence, never a 500.
  rm -rf "$wt"
  fetch "${name}_missing" GET "/api/sessions/$id/changes" "Host: 127.0.0.1" ||
    fail "${name}_missing/no_answer" "GET /api/sessions/$id/changes did not answer"
  response="$(answer "${name}_missing")"
  expect_status "${name}_missing" "$response" 409
  expect_code "${name}_missing" "$response" no_worktree
  expect_header "${name}_missing" "$response" "Content-Type" "application/json"
  # ... and a pruned one: git forgets the registration, and a directory that is there anyway is not a worktree.
  git -C "$repo" worktree prune || fail "${name}/prune" "git worktree prune did not work"
  mkdir -p "$wt"
  fetch "${name}_pruned" GET "/api/sessions/$id/changes" "Host: 127.0.0.1" ||
    fail "${name}_pruned/no_answer" "GET /api/sessions/$id/changes did not answer"
  response="$(answer "${name}_pruned")"
  expect_status "${name}_pruned" "$response" 409
  expect_code "${name}_pruned" "$response" no_worktree
  body="$(cat "$WORK/responses/${name}_pruned.body")"
  printf '%s\n' "$body" | grep -qF 'resume' ||
    fail "${name}_pruned/sentence" "the sentence names no next step: $body"
  assertions=$((assertions + 1))

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# the Changes sheet is **shipped**. No gate may need a browser, so this case measures the three halves a
# browser would otherwise be the only witness of:
#
#   1. the sheet's screen is still the app shell, byte-for-byte (`case_session_screen`'s comparison, on a session
#      whose changes exist) — a `dist/` built before the sheet fails here;
#   2. the bundle the artifact **serves** carries the sheet's element ids and the three sentences the ADRs fixed, so
#      a tree that dropped a rule, or an artifact built before it, is red;
#   3. the changes answer carries the pickers' options — the one key this story added — with the configured targets
#      the repository has (in the config's order) and the publish remote passed through verbatim.
#
# The sheet's *behaviour* is `web/test/transcript.test.ts`'s: `npm test` is the only gate that can see a sentence,
# and a rule that only the DOM could show would not be a gate case at all (the gate rule).
case_changes_screen() {
  local name=case_changes_screen response body id base wt asset needle
  # Two statements, not one, for the reason `case_changes` gives: in a single `local` every right-hand side expands
  # before either assignment, so `repo="$projects/…"` on the same line reads an unset `projects` and `set -u` stops.
  local projects="$WORK/projects-screen"
  local repo="$projects/holosmith"
  # The guard comes first, before `stop_serve`: a skipped case that stopped the server leaves the phase's own
  # teardown with no pid to signal.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to answer the docker preflight (web/test/transcript.test.ts covers the rules)\n' "$name"
    return 0
  fi
  stop_serve
  mkdir -p "$repo"
  git -C "$repo" init --quiet -b main || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet --allow-empty \
    -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  base="$(git -C "$repo" rev-parse HEAD)" || fail "$name/git" "git rev-parse did not work in $repo"
  # The default `[merge] targets` is `["main", "master"]`, and a repository has one of them — so the second branch
  # is what makes the picker's list more than its preselection: both exist here, and the order is the config's.
  git -C "$repo" branch master || fail "$name/git" "git branch master did not work in $repo"

  socket_pid="$(fake_docker_ping "$WORK/docker-screen.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker-screen.sock" ] && break
    sleep 0.05
  done
  # A publish remote is configured, and no repository here has a remote by that name: the list is the config's,
  # unfiltered (ADR-0067), and that is the arm this asserts.
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-screen.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n[publish]\nremotes = ["origin"]\n' \
    "$owner" "$projects" "$WORK" >"$WORK/config-screen.toml"
  start_serve "$WORK/config-screen.toml" "$WORK/data-screen"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-screen-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"

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
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_row/base" "the row does not record the branch's base commit: $body"
  assertions=$((assertions + 1))

  wt="$projects/.worktrees/$id"
  # The provisioner took back what its worktree step made (ADR-0029), so the case makes it again with the step's own
  # command — absolute paths, because `worktree add` resolves a relative one against the repository.
  git -C "$repo" worktree add --quiet -b "agent/$id" "$wt" "$base" ||
    fail "${name}/worktree" "git worktree add did not work for $wt"
  # The file-name arm of `CI_PATHS`, so the answer says `touches_ci: true` and the sheet's warning has something to
  # show; a `.github/` prefix would measure the other arm.
  printf 'pipeline {}\n' >"$wt/Jenkinsfile" || fail "${name}/write" "the session's file is not writable"
  git -C "$wt" add Jenkinsfile || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the session's change" ||
    fail "${name}/commit" "git commit did not work in $wt"

  # 1. The screen is the shell, byte for byte.
  fetch "$name" GET "/sessions/$id" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /sessions/$id did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  [ -f web/dist/index.html ] ||
    fail "$name/index_html" "web/dist/index.html is missing: run scripts/build.sh"
  cmp -s "$WORK/responses/$name.body" web/dist/index.html ||
    fail "$name/index_html" "the screen path is not this tree's web/dist/index.html"
  assertions=$((assertions + 1))

  # 2. The bundle the artifact serves: the file the build made, and the sheet's own strings inside it.
  asset="$(sed -n 's/.*src="\([^"]*\.js\)".*/\1/p' web/dist/index.html | head -1)"
  [ -n "$asset" ] || fail "$name/asset" "the shell names no script to fetch"
  fetch "${name}_asset" GET "$asset" "Host: 127.0.0.1" ||
    fail "${name}_asset/no_answer" "GET $asset did not answer"
  response="$(answer "${name}_asset")"
  expect_status "${name}_asset" "$response" 200
  cmp -s "$WORK/responses/${name}_asset.body" "web/dist$asset" ||
    fail "${name}_asset/bytes" "the served bundle is not this tree's web/dist$asset"
  assertions=$((assertions + 1))
  # One id per element the sheet is made of, and the three sentences a decision already fixed: the CI warning
  # (ADR-0066 D11), the commit prompt (ADR-0065 D4) and the Resolve prompt (ADR-0064 D10's affordance, spelled by
  # ). A `for` over needles rather than fourteen `grep`s, and every needle counted so a needle that found
  # nothing cannot pass as one that did.
  # Each needle names the assignment's own quotes (`id="changes-conflict"`), not the bare word: `changes-conflict`
  # is a prefix of `changes-conflict-text` and `changes-publish` one of `changes-publish-start`, so a bare word
  # would still pass with the container element deleted — the opposite of what this loop is for.
  for needle in \
    'id="changes-toggle"' 'id="changes-body"' 'id="changes-counts"' 'id="changes-warning"' 'id="changes-files"' \
    'id="changes-diff"' 'id="changes-target"' 'id="changes-strategy"' 'id="changes-merge"' 'id="changes-reason"' \
    'id="changes-commit"' 'id="changes-conflict"' 'id="changes-resolve"' 'id="changes-publish"' \
    'id="changes-confirm"' 'id="changes-conflict-text"' 'id="changes-conflict-paths"' 'id="changes-remote"' \
    'id="changes-question"' \
    'this branch changes CI configuration, so publishing it may run workflows in the remote repository' \
    "Commit every change in the working tree, with a message that describes it, and tell me the commit's hash." \
    "Merge the target branch into your branch and resolve the conflicts, then commit the result and tell me the commit's hash."; do
    grep -qF -- "$needle" "$WORK/responses/${name}_asset.body" ||
      fail "${name}_asset/sheet" "the served bundle does not carry $needle"
    assertions=$((assertions + 1))
  done

  # 3. The answer the sheet draws, including the one key this story added.
  fetch "${name}_changes" GET "/api/sessions/$id/changes" "Host: 127.0.0.1" ||
    fail "${name}_changes/no_answer" "GET /api/sessions/$id/changes did not answer"
  response="$(answer "${name}_changes")"
  expect_status "${name}_changes" "$response" 200
  expect_header "${name}_changes" "$response" "Content-Type" "application/json"
  body="$(cat "$WORK/responses/${name}_changes.body")"
  # Both existing configured targets, in the config's order, and the remote verbatim: the pickers' options.
  printf '%s\n' "$body" | grep -qF '"choices":{"targets":["main","master"],"remotes":["origin"]}' ||
    fail "${name}_changes/choices" "the answer does not carry the pickers' options: $body"
  assertions=$((assertions + 1))
  # `target` is the first of that list, which is what the target picker preselects and what the counts are about.
  printf '%s\n' "$body" | grep -qF '"target":"main"' ||
    fail "${name}_changes/target" "the preselected target is not the first configured one: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"touches_ci":true' ||
    fail "${name}_changes/touches_ci" "a Jenkinsfile did not raise the CI flag: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"path":"Jenkinsfile"' ||
    fail "${name}_changes/files" "the file list does not carry the session's file: $body"
  assertions=$((assertions + 1))

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# the proof from the artifact: the unified diff itself. One real repository under a temp projects root and
# a `_ping`-only socket (no daemon, which is what item 12 asks for — "all git work for review and merge runs on
# the host"), a session whose worktree the provisioner took back and this case re-makes with the step's own
# command, and three commits on `agent/<id>`: a text file, a NUL-bearing binary, and one large enough that its
# patch is past the 256 KB cap.
#
# Every byte this case accepts is compared with `git diff` run by the case itself, so the artifact cannot pass
# by agreeing with its own arithmetic; the two refusals are asserted as status **and** code **and** sentence,
# because a path that leaked would still be a 404 for a file that is not there. It boots its own server for the
# reason `case_changes` does: the worktree it asserts is the one the failed network step took back.
case_changes_diff() {
  local name=case_changes_diff response body id base tip wt size counted marker attempted label n
  # Two statements, not one, for the reason `case_changes` gives: in a single `local` every right-hand side
  # expands before either assignment, so `repo="$projects/..."` would read an unset `projects` under `set -u`.
  local projects="$WORK/projects-diff"
  local repo="$projects/holosmith"
  # The guards come first, before `stop_serve`: a skipped case that stopped the server leaves the phase's own
  # teardown with no pid to signal.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to answer the docker preflight (tests/changes.rs covers this)\n' "$name"
    return 0
  fi
  stop_serve
  mkdir -p "$repo"
  git -C "$repo" init --quiet -b main || fail "$name/git" "git init did not work in $repo"
  # A file the base holds and the branch does not touch: the sharpest `path` refusal, because git *would*
  # answer an empty patch for it and only the changed-file list knows there is nothing to show.
  printf 'kept\n' >"$repo/keep.txt" || fail "$name/write" "the file is not writable"
  git -C "$repo" add keep.txt || fail "$name/add" "git add did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet -m "the first commit" ||
    fail "$name/git" "git commit did not work in $repo"
  base="$(git -C "$repo" rev-parse HEAD)" || fail "$name/git" "git rev-parse did not work in $repo"

  socket_pid="$(fake_docker_ping "$WORK/docker-diff.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker-diff.sock" ] && break
    sleep 0.05
  done
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-diff.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$projects" "$WORK" >"$WORK/config-diff.toml"
  start_serve "$WORK/config-diff.toml" "$WORK/data-diff"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-diff-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
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
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_row/base" "the row does not record the branch's base commit: $body"

  wt="$projects/.worktrees/$id"
  # The step's own command, absolute paths, because `worktree add` resolves a relative one against the
  # repository — a fixture, not a re-implementation of the step.
  git -C "$repo" worktree add --quiet -b "agent/$id" "$wt" "$base" ||
    fail "${name}/worktree" "git worktree add did not work for $wt"
  printf 'one\ntwo\n' >"$wt/a.txt" || fail "${name}/write" "the session's file is not writable"
  git -C "$wt" add a.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the text change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  # A NUL in the first 8000 bytes is git's whole definition of a binary file.
  printf '\000\001\002binary\n' >"$wt/bin.dat" || fail "${name}/write" "the binary file is not writable"
  git -C "$wt" add bin.dat || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the binary" ||
    fail "${name}/commit" "git commit did not work in $wt"
  # ~520 KB of patch: past the 256 KB cap with room to spare, and well under `changes::COUNT_MAX` (8 MiB), so
  # the marker's total is an exact count rather than a lower bound. Written with a shell loop rather than `seq`,
  # because a fixture must not be the reason this case skips: `seq` is not the feature, and the wait loops below
  # already assume coreutils on a machine that runs the gate at all.
  n=0
  : >"$wt/big.txt" || fail "${name}/write" "the large file is not writable"
  while [ "$n" -lt 20000 ]; do
    printf 'the agent wrote this line\n' >>"$wt/big.txt" || fail "${name}/write" "the large file is not writable"
    n=$((n + 1))
  done
  git -C "$wt" add big.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the large file" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"

  # 1. One text file: the bytes are git's own, compared with the command the module runs.
  fetch "${name}_file" GET "/api/sessions/$id/changes/diff?path=a.txt" "Host: 127.0.0.1" ||
    fail "${name}_file/no_answer" "GET /api/sessions/$id/changes/diff?path=a.txt did not answer"
  response="$(answer "${name}_file")"
  expect_status "${name}_file" "$response" 200
  expect_header "${name}_file" "$response" "Content-Type" "text/plain; charset=utf-8"
  git -C "$repo" diff --no-ext-diff --no-textconv --no-renames --no-color "$base" "$tip" -- a.txt \
    >"$WORK/responses/$name.git-a.txt" || fail "${name}/git" "git diff did not work in $repo"
  cmp -s "$WORK/responses/${name}_file.body" "$WORK/responses/$name.git-a.txt" ||
    fail "${name}_file/bytes" "the answer is not git's own diff of a.txt"
  assertions=$((assertions + 1))
  grep -qF '+two' "$WORK/responses/${name}_file.body" ||
    fail "${name}_file/hunk" "the answer carries no hunk of the change (${name}_file.body)"
  assertions=$((assertions + 1))

  # 2. The whole branch, with no `path`: capped like any other answer, and it names the first file alphabetically.
  fetch "${name}_branch" GET "/api/sessions/$id/changes/diff" "Host: 127.0.0.1" ||
    fail "${name}_branch/no_answer" "GET /api/sessions/$id/changes/diff did not answer"
  response="$(answer "${name}_branch")"
  expect_status "${name}_branch" "$response" 200
  expect_header "${name}_branch" "$response" "Content-Type" "text/plain; charset=utf-8"
  # Read from the file, not through `printf | grep -q` (see `expect_header`): a matched `grep -q` closes the
  # pipe under a `printf` that is still writing 256 KB, and `pipefail` turns that SIGPIPE into a 141 — a
  # passing assertion reported as a failure (measured, this case's first run).
  grep -qF 'diff --git a/a.txt b/a.txt' "$WORK/responses/${name}_branch.body" ||
    fail "${name}_branch/names" "the branch patch names no file (the answer is in ${name}_branch.body)"
  assertions=$((assertions + 1))
  size="$(wc -c <"$WORK/responses/${name}_branch.body")"
  [ "$size" -le 262144 ] || fail "${name}_branch/cap" "the whole-branch answer is $size bytes, past the cap"
  assertions=$((assertions + 1))
  marker="$(tail -1 "$WORK/responses/${name}_branch.body")"
  case "$marker" in
    '[diff truncated:'*) ;;
    *) fail "${name}_branch/marker" "a capped answer ends with no marker: $marker" ;;
  esac
  assertions=$((assertions + 1))

  # 3. The binary file: git's own summary, and not one NUL byte on the wire.
  fetch "${name}_binary" GET "/api/sessions/$id/changes/diff?path=bin.dat" "Host: 127.0.0.1" ||
    fail "${name}_binary/no_answer" "the binary diff did not answer"
  response="$(answer "${name}_binary")"
  expect_status "${name}_binary" "$response" 200
  body="$(cat "$WORK/responses/${name}_binary.body")"
  printf '%s\n' "$body" | grep -qF 'Binary files /dev/null and b/bin.dat differ' ||
    fail "${name}_binary/summary" "the binary file was not summarised: $body"
  assertions=$((assertions + 1))
  size="$(wc -c <"$WORK/responses/${name}_binary.body")"
  counted="$(tr -d '\000' <"$WORK/responses/${name}_binary.body" | wc -c)"
  [ "$size" = "$counted" ] || fail "${name}_binary/nul" "a NUL byte reached the answer"
  assertions=$((assertions + 1))

  # 4. A file past the cap: at most 256 KB, and the marker names what git really printed.
  fetch "${name}_large" GET "/api/sessions/$id/changes/diff?path=big.txt" "Host: 127.0.0.1" ||
    fail "${name}_large/no_answer" "the large diff did not answer"
  response="$(answer "${name}_large")"
  expect_status "${name}_large" "$response" 200
  size="$(wc -c <"$WORK/responses/${name}_large.body")"
  [ "$size" -le 262144 ] || fail "${name}_large/cap" "the answer is $size bytes, past the cap"
  assertions=$((assertions + 1))
  counted="$(git -C "$repo" diff --no-ext-diff --no-textconv --no-renames --no-color "$base" "$tip" -- big.txt | wc -c)" ||
    fail "${name}/git" "git diff did not work in $repo"
  [ "$counted" -gt 262144 ] || fail "${name}_large/fixture" "the fixture's patch is only $counted bytes"
  assertions=$((assertions + 1))
  marker="$(tail -1 "$WORK/responses/${name}_large.body")"
  case "$marker" in
    *'diff is '"$counted"' bytes'*) ;;
    *) fail "${name}_large/marker" "the marker does not name git's $counted bytes: $marker" ;;
  esac
  assertions=$((assertions + 1))

  # 5. A traversal and an unchanged path: the same 404, and neither reaches git.
  for attempted in '..%2Fetc%2Fpasswd' '%2Fetc%2Fpasswd' 'keep.txt'; do
    label="${name}_miss_$(printf '%s' "$attempted" | tr -c 'a-zA-Z0-9' '_')"
    fetch "$label" GET "/api/sessions/$id/changes/diff?path=$attempted" "Host: 127.0.0.1" ||
      fail "$label/no_answer" "a refused path did not answer"
    response="$(answer "$label")"
    expect_status "$label" "$response" 404
    expect_code "$label" "$response" not_found
    if grep -qF 'root:' "$WORK/responses/$label.body"; then
      fail "$label/leak" "the answer carried a file's contents"
    fi
    assertions=$((assertions + 1))
  done

  # 6. The worktree gone: the summary's own `409 no_worktree`, which is the fact a client already knows how to
  # read — never a 500 from a git asked about a directory that is not there.
  rm -rf "$wt"
  fetch "${name}_gone" GET "/api/sessions/$id/changes/diff?path=a.txt" "Host: 127.0.0.1" ||
    fail "${name}_gone/no_answer" "the gone-worktree diff did not answer"
  response="$(answer "${name}_gone")"
  expect_status "${name}_gone" "$response" 409
  expect_code "${name}_gone" "$response" no_worktree
  body="$(cat "$WORK/responses/${name}_gone.body")"
  printf '%s\n' "$body" | grep -qF 'resume' ||
    fail "${name}_gone/sentence" "the sentence names no next step: $body"
  assertions=$((assertions + 1))

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# the proof from the artifact: spec item 12's rules 2 and 4, and the first route in this build whose work
# **writes to the project repository**. One real repository under a temp projects root and a `_ping`-only socket
# (no daemon, which is item 12's own shape — "all git work for review and merge runs on the host"), a session
# whose worktree the provisioner took back and this case re-makes with the step's own command.
#
# The two update paths rule 4 names are measured **separately**, because they differ in what moves: a detached
# checkout makes the target unchecked-out (`git rev-parse main` moves, the checkout's file does not appear), and
# the ordinary checkout is then restored so the fast-forward moves the operator's working tree too. Every ref this
# case accepts is compared with `git rev-parse` run by the case, so the artifact cannot pass by agreeing with its
# own arithmetic, and the four refusals are asserted as status **and** code with the target unmoved across them.
case_merge() {
  local name=case_merge response body id base tip tip2 tip3 main wt
  # Two statements, not one, for the reason `case_changes` gives: in a single `local` every right-hand side
  # expands before either assignment, so `repo="$projects/holosmith"` would read an unset `projects` under
  # `set -u`.
  local projects="$WORK/projects-merge"
  local repo="$projects/holosmith"
  # The guard comes first, before `stop_serve`, for the reason `case_worktree` gives: a skipped case that
  # stopped the server leaves the phase's own teardown with no pid to signal.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to answer the docker preflight (tests/merge.rs covers this)\n' "$name"
    return 0
  fi
  stop_serve
  mkdir -p "$repo"
  git -C "$repo" init --quiet -b main || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet --allow-empty \
    -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  base="$(git -C "$repo" rev-parse HEAD)" || fail "$name/git" "git rev-parse did not work in $repo"

  socket_pid="$(fake_docker_ping "$WORK/docker-merge.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker-merge.sock" ] && break
    sleep 0.05
  done
  # `trunk` is in the list and not in the repository: the second `bad_target` fact and the ordinary shape of the
  # default `["main", "master"]` list, which names a branch half the machines do not have.
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-merge.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n[merge]\ntargets = ["main", "trunk"]\n' \
    "$owner" "$projects" "$WORK" >"$WORK/config-merge.toml"
  start_serve "$WORK/config-merge.toml" "$WORK/data-merge"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-merge-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
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
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_row/base" "the row does not record the branch's base commit: $body"

  wt="$projects/.worktrees/$id"
  # The step's own command, absolute paths, because `worktree add` resolves a relative one against the
  # repository — a fixture, not a re-implementation of the step.
  git -C "$repo" worktree add --quiet -b "agent/$id" "$wt" "$base" ||
    fail "${name}/worktree" "git worktree add did not work for $wt"
  printf 'one\n' >"$wt/a.txt" || fail "${name}/write" "the session's file is not writable"
  git -C "$wt" add a.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the session's change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"

  # 1. **The unchecked-out path**: rule 4's first arm. Detaching the project's checkout is what makes `main` a
  # ref no working tree holds, so the merge must move the ref and *nothing else* — an implementation that always
  # ran `git merge --ff-only` somewhere would move a working tree here.
  git -C "$repo" checkout --quiet --detach "$base" || fail "${name}/detach" "git checkout --detach did not work"
  fetch_body "${name}_unchecked" POST "/api/sessions/$id/merge" '{"target":"main"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_unchecked/no_answer" "POST /api/sessions/$id/merge did not answer"
  response="$(answer "${name}_unchecked")"
  expect_status "${name}_unchecked" "$response" 200
  expect_header "${name}_unchecked" "$response" "Content-Type" "application/json"
  body="$(cat "$WORK/responses/${name}_unchecked.body")"
  printf '%s\n' "$body" | grep -qF "\"merged_commit\":\"$tip\"" ||
    fail "${name}_unchecked/commit" "the answer does not name the branch's tip: $body"
  assertions=$((assertions + 1))
  # A body with no `strategy` is `ff-only`, which is item 12's default rather than an echo of the request.
  printf '%s\n' "$body" | grep -qF '"strategy":"ff-only"' ||
    fail "${name}_unchecked/strategy" "the default strategy is not ff-only: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"target":"main"' ||
    fail "${name}_unchecked/target" "the answer names no target: $body"
  assertions=$((assertions + 1))
  check "${name}_unchecked/ref" "$tip" "$(git -C "$repo" rev-parse refs/heads/main)"
  # ... and the checkout did not move: it is still detached at the base and the agent's file is not in it.
  check "${name}_unchecked/checkout" "$base" "$(git -C "$repo" rev-parse HEAD)"
  if [ -e "$repo/a.txt" ]; then
    fail "${name}_unchecked/untouched" "the unchecked-out merge wrote into the detached checkout"
  fi
  assertions=$((assertions + 1))

  # 2. **The refusals that move nothing**, with the target compared across all of them. `squash` used to be one of
  # these (`422 unsupported`) and is a strategy, and `delete_session` left the list when built
  # it (its arm is `case_merge_guards`'s, because it needs a repository to delete from). What remains is the
  # parameter's own facts: two `bad_target`s and a fourth strategy word.
  main="$(git -C "$repo" rev-parse refs/heads/main)"
  for pair in '{"target":"nope"}|bad_target' '{"target":"trunk"}|bad_target' \
    '{"target":"main","strategy":"rebase"}|bad_strategy'; do
    fixture_body="${pair%|*}"
    fixture_code="${pair##*|}"
    label="${name}_refused_$fixture_code"
    label="$label$(printf '%s' "$fixture_body" | tr -c 'a-z0-9' '_')"
    fetch_body "$label" POST "/api/sessions/$id/merge" "$fixture_body" \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "$label/no_answer" "a refused merge did not answer"
    response="$(answer "$label")"
    expect_status "$label" "$response" 422
    expect_code "$label" "$response" "$fixture_code"
    expect_header "$label" "$response" "Content-Type" "application/json"
  done
  check "${name}_refused/unmoved" "$main" "$(git -C "$repo" rev-parse refs/heads/main)"

  # 3. **The ordinary path**: rule 4's second arm. `main` is checked out in the project's own checkout again, so
  # the fast-forward runs there and the operator's working tree moves with the ref.
  git -C "$repo" checkout --quiet main || fail "${name}/checkout" "git checkout main did not work"
  printf 'two\n' >"$wt/c.txt" || fail "${name}/write" "the session's second file is not writable"
  git -C "$wt" add c.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the session's second change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip2="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  fetch_body "${name}_checked" POST "/api/sessions/$id/merge" '{"target":"main","strategy":"ff-only"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_checked/no_answer" "the checked-out merge did not answer"
  response="$(answer "${name}_checked")"
  expect_status "${name}_checked" "$response" 200
  body="$(cat "$WORK/responses/${name}_checked.body")"
  printf '%s\n' "$body" | grep -qF "\"merged_commit\":\"$tip2\"" ||
    fail "${name}_checked/commit" "the answer does not name the second tip: $body"
  assertions=$((assertions + 1))
  check "${name}_checked/ref" "$tip2" "$(git -C "$repo" rev-parse refs/heads/main)"
  # The difference between the two update paths, measured: *this* time the file is on disk in the checkout.
  if [ ! -e "$repo/c.txt" ]; then
    fail "${name}_checked/working_tree" "the fast-forward did not move the checkout's working tree"
  fi
  assertions=$((assertions + 1))
  check "${name}_checked/clean" "" "$(git -C "$repo" status --porcelain)"

  # 4. **`target_dirty`**: the branch adds a file the checkout already holds untracked, so git refuses to
  # overwrite the operator's work. Nothing moves and the operator's file is untouched.
  printf 'three\n' >"$wt/d.txt" || fail "${name}/write" "the session's third file is not writable"
  git -C "$wt" add d.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the session's third change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip3="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  printf 'the operator\x27s work in progress\n' >"$repo/d.txt" ||
    fail "${name}/write" "the operator's file is not writable"
  fetch_body "${name}_dirty" POST "/api/sessions/$id/merge" '{"target":"main"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_dirty/no_answer" "the dirty merge did not answer"
  response="$(answer "${name}_dirty")"
  expect_status "${name}_dirty" "$response" 409
  expect_code "${name}_dirty" "$response" target_dirty
  body="$(cat "$WORK/responses/${name}_dirty.body")"
  printf '%s\n' "$body" | grep -qF 'nothing was merged' ||
    fail "${name}_dirty/sentence" "the sentence does not say nothing happened: $body"
  assertions=$((assertions + 1))
  check "${name}_dirty/ref" "$tip2" "$(git -C "$repo" rev-parse refs/heads/main)"
  check "${name}_dirty/operator_file" "the operator's work in progress" "$(cat "$repo/d.txt")"

  # ... and the same merge with the file out of the way lands, which is what makes the refusal the file's fault.
  rm -f "$repo/d.txt" || fail "${name}/rm" "the operator's file is not removable"
  fetch_body "${name}_retry" POST "/api/sessions/$id/merge" '{"target":"main"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_retry/no_answer" "the retried merge did not answer"
  response="$(answer "${name}_retry")"
  expect_status "${name}_retry" "$response" 200
  check "${name}_retry/ref" "$tip3" "$(git -C "$repo" rev-parse refs/heads/main)"

  # 5. **`not_fast_forward`**: the target moves on, so there is nothing to fast-forward to. Both refs and the
  # checkout are unchanged — "nothing was merged" measured rather than asserted from a sentence.
  printf 'the target\n' >"$repo/t.txt" || fail "${name}/write" "the target's file is not writable"
  git -C "$repo" add t.txt || fail "${name}/add" "git add did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet -m "the target moves on" ||
    fail "${name}/commit" "git commit did not work in $repo"
  main="$(git -C "$repo" rev-parse refs/heads/main)"
  fetch_body "${name}_behind" POST "/api/sessions/$id/merge" '{"target":"main"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_behind/no_answer" "the refused merge did not answer"
  response="$(answer "${name}_behind")"
  expect_status "${name}_behind" "$response" 409
  expect_code "${name}_behind" "$response" not_fast_forward
  body="$(cat "$WORK/responses/${name}_behind.body")"
  printf '%s\n' "$body" | grep -qF 'nothing was merged' ||
    fail "${name}_behind/sentence" "the sentence does not say nothing happened: $body"
  assertions=$((assertions + 1))
  check "${name}_behind/ref" "$main" "$(git -C "$repo" rev-parse refs/heads/main)"
  check "${name}_behind/branch" "$tip3" "$(git -C "$repo" rev-parse "agent/$id")"
  check "${name}_behind/checkout" "" "$(git -C "$repo" status --porcelain)"

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# the proof from the artifact: the two strategies that make a commit, and rule 3's conflict, over **one**
# repository. It boots its own Docker run over its own projects root and data root for the reason `case_merge`
# does — a case here must own the repository it moves — and it re-makes the worktree the failed network step took
# back with the step's own command.
#
# The three phases are one session's history, in order, because that is the shape the operator's is:
#   * a **squash** of three commits is *one* commit whose parent is the old target and whose tree is the
#     branch's — the tree, because `git log --oneline` cannot tell one commit from one commit that lost a hunk;
#   * a **merge** of two more commits is *one* commit with two parents, the target and the branch's tip;
#   * a **conflict** on one path is a `409 conflict` whose body names it, with the target unmoved and the
#     checkout's status untouched.
# `<data>/merge` is measured empty after every one of them: rule 2 says the temporary worktree is removed
# afterwards, and "afterwards" includes a refusal.
case_merge_strategies() {
  local name=case_merge_strategies response body id base wt tip main squashed parents merge_dir
  # Two statements, not one, for the reason `case_changes` gives: in a single `local` every right-hand side
  # expands before either assignment, so `repo="$projects/holosmith"` would read an unset `projects` under
  # `set -u`.
  local projects="$WORK/projects-strategies"
  local data="$WORK/data-strategies"
  local repo="$projects/holosmith"
  merge_dir="$data/merge"
  # The guard comes first, before `stop_serve`, for the reason `case_worktree` gives.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to answer the docker preflight (tests/merge.rs covers this)\n' "$name"
    return 0
  fi
  stop_serve
  mkdir -p "$repo"
  git -C "$repo" init --quiet -b main || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet --allow-empty \
    -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  base="$(git -C "$repo" rev-parse HEAD)" || fail "$name/git" "git rev-parse did not work in $repo"

  socket_pid="$(fake_docker_ping "$WORK/docker-strategies.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker-strategies.sock" ] && break
    sleep 0.05
  done
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-strategies.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n[merge]\ntargets = ["main"]\n' \
    "$owner" "$projects" "$WORK" >"$WORK/config-strategies.toml"
  start_serve "$WORK/config-strategies.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-strategies-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
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
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_row/base" "the row does not record the branch's base commit: $body"

  wt="$projects/.worktrees/$id"
  # The step's own command, absolute paths, because the failed network step took the
  # worktree back — a fixture, not a re-implementation of the step.
  git -C "$repo" worktree add --quiet -b "agent/$id" "$wt" "$base" ||
    fail "${name}/worktree" "git worktree add did not work for $wt"
  for file in one two three; do
    printf '%s\n' "$file" >"$wt/$file.txt" || fail "${name}/write" "$file.txt is not writable"
    git -C "$wt" add "$file.txt" || fail "${name}/add" "git add did not work in $wt"
    git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the $file change" ||
      fail "${name}/commit" "git commit did not work in $wt"
  done
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"

  # 1. **`squash`**: three commits become one. The target's new commit has **one** parent, the old target, and the
  # branch's **tree** — one commit that lost a hunk is still one commit with one parent, so the tree is what has
  # to be compared.
  fetch_body "${name}_squash" POST "/api/sessions/$id/merge" '{"target":"main","strategy":"squash"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_squash/no_answer" "POST /api/sessions/$id/merge did not answer"
  response="$(answer "${name}_squash")"
  expect_status "${name}_squash" "$response" 200
  expect_header "${name}_squash" "$response" "Content-Type" "application/json"
  body="$(cat "$WORK/responses/${name}_squash.body")"
  squashed="$(git -C "$repo" rev-parse refs/heads/main)" || fail "${name}/rev" "git rev-parse did not work"
  printf '%s\n' "$body" | grep -qF "\"merged_commit\":\"$squashed\"" ||
    fail "${name}_squash/commit" "the answer does not name where the target is: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"strategy":"squash"' ||
    fail "${name}_squash/strategy" "the answer does not name the strategy that ran: $body"
  assertions=$((assertions + 1))
  check "${name}_squash/count" "1" "$(git -C "$repo" rev-list --count "$base..refs/heads/main")"
  check "${name}_squash/parent" "$base" "$(git -C "$repo" rev-parse refs/heads/main^)"
  check "${name}_squash/tree" "$(git -C "$wt" rev-parse "agent/$id^{tree}")" \
    "$(git -C "$repo" rev-parse "refs/heads/main^{tree}")"
  # The working tree moved with the ref: the ordinary path.
  if [ ! -e "$repo/three.txt" ]; then
    fail "${name}_squash/working_tree" "the squash did not move the checkout's working tree"
  fi
  assertions=$((assertions + 1))
  check "${name}_squash/clean" "" "$(git -C "$repo" status --porcelain)"

  # 2. **`merge`**: two more commits on the branch, and the target gets a commit with **two** parents — the
  # squashed target and the branch's tip. This is the assertion `strategy:"merge"` exists for, and the one an
  # implementation that quietly fast-forwarded cannot pass.
  for file in four five; do
    printf '%s\n' "$file" >"$wt/$file.txt" || fail "${name}/write" "$file.txt is not writable"
    git -C "$wt" add "$file.txt" || fail "${name}/add" "git add did not work in $wt"
    git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the $file change" ||
      fail "${name}/commit" "git commit did not work in $wt"
  done
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  fetch_body "${name}_merge" POST "/api/sessions/$id/merge" '{"target":"main","strategy":"merge"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_merge/no_answer" "the merge did not answer"
  response="$(answer "${name}_merge")"
  expect_status "${name}_merge" "$response" 200
  body="$(cat "$WORK/responses/${name}_merge.body")"
  printf '%s\n' "$body" | grep -qF '"strategy":"merge"' ||
    fail "${name}_merge/strategy" "the answer does not name the strategy that ran: $body"
  assertions=$((assertions + 1))
  parents="$(git -C "$repo" rev-list --parents -n 1 refs/heads/main)" ||
    fail "${name}/rev" "git rev-list did not work"
  check "${name}_merge/parents" "$(git -C "$repo" rev-parse refs/heads/main) $squashed $tip" "$parents"
  check "${name}_merge/clean" "" "$(git -C "$repo" status --porcelain)"

  # 3. **The conflict**: one path touched on both sides. `409 conflict`, the path in the body's `conflicts`, the
  # target unmoved, the checkout's status untouched — and the temporary directory empty, because the abort *is*
  # the worktree's removal.
  printf 'the target\x27s line\n' >"$repo/f.txt" || fail "${name}/write" "f.txt is not writable in the repo"
  git -C "$repo" add f.txt || fail "${name}/add" "git add did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet -m "the target's line" ||
    fail "${name}/commit" "git commit did not work in $repo"
  printf 'the branch\x27s line\n' >"$wt/f.txt" || fail "${name}/write" "f.txt is not writable in the worktree"
  git -C "$wt" add f.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the branch's line" ||
    fail "${name}/commit" "git commit did not work in $wt"
  main="$(git -C "$repo" rev-parse refs/heads/main)" || fail "${name}/rev" "git rev-parse did not work"
  fetch_body "${name}_conflict" POST "/api/sessions/$id/merge" '{"target":"main","strategy":"merge"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_conflict/no_answer" "the conflicting merge did not answer"
  response="$(answer "${name}_conflict")"
  expect_status "${name}_conflict" "$response" 409
  expect_code "${name}_conflict" "$response" conflict
  body="$(cat "$WORK/responses/${name}_conflict.body")"
  printf '%s\n' "$body" | grep -qF '"conflicts":["f.txt"]' ||
    fail "${name}_conflict/paths" "the body does not name the conflicting path: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF 'nothing was merged' ||
    fail "${name}_conflict/sentence" "the sentence does not say nothing happened: $body"
  assertions=$((assertions + 1))
  check "${name}_conflict/unmoved" "$main" "$(git -C "$repo" rev-parse refs/heads/main)"
  check "${name}_conflict/checkout" "" "$(git -C "$repo" status --porcelain)"
  check "${name}_conflict/operator_file" "the target's line" "$(cat "$repo/f.txt")"

  # Rule 2's last sentence, measured after all three: `<data>/merge` exists and holds nothing. `find` rather than
  # an inference from a status code — a worktree left behind is exactly what an `ls` would show.
  if [ -d "$merge_dir" ]; then
    check "${name}/temporary" "0" "$(find "$merge_dir" -mindepth 1 | wc -l | tr -d ' ')"
  else
    fail "${name}/temporary" "the merge never made $merge_dir, so this case measured nothing"
  fi

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# `set_row_status <database> <id> <status>`: the one write a case makes by hand.
#
# Rule 1's first guard is about a row whose turn is running, and this gate cannot run a turn: its Docker boot is a
# `_ping` shim, so a session's network step fails and no agent is ever launched. What the guard reads is one
# column, so the column is what the case writes — with `sqlite3` from `python3`, the way `case_exit_classification`
# reads `expected_stop` out of the same file. The server's connection is in WAL, so a committed write from another
# process is what its next read sees. `tests/merge.rs` drives the same row through the server's own table, which is
# why this shortcut is the gate's and not the code's.
set_row_status() {
  python3 - "$1" "$2" "$3" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1], timeout=5)
connection.execute("UPDATE sessions SET status = ?, status_reason = NULL WHERE id = ?",
                   (sys.argv[3], sys.argv[2]))
connection.commit()
connection.close()
PY
}

# the proof from the artifact: rule 1's two guards and rule 5's `delete_session`, over its own repository,
# projects root and **data root** — `<data>/merge` is what a refused merge must not make and what a landing must
# leave empty — and its own `_ping` socket. Six arms, in the order each one needs:
#
#   * an untracked file in the session's own worktree is `409 dirty_worktree`, with the target, the branch and the
#     file untouched and no `<data>/merge` at all. The file is *untracked* on purpose: that is `changes::dirty`'s
#     definition of work, so the guard cannot be a `status --ignored` or an `ls-files --others`;
#   * the same file **ignored** by a committed `.gitignore` does not refuse — every build writes ignored files;
#   * a row whose turn is running is `409 session_running` (its one column written by hand), and the same row in
#     the `error` state its failed provision left **merges**: the negative half, which an `!= Idle` guard would
#     refuse;
#   * `delete_session: true` after a **squash** answers `200` with rule 6's three keys and then removes the row,
#     the worktree and `agent/<id>` — the branch is deleted *because this request merged it*, which the ordinary
#     keep-rule would not do for a squash (ADR-0064 D11, ADR-0065 D6);
#   * a second session's squash **without** the flag keeps the branch, and the ordinary `DELETE` afterwards answers
#     `kept.branch` naming it — so the flag is what deleted the first one;
#   * a `delete_session` whose delete cannot snapshot (its `sessions` directory made read-only) still answers `200`
#     with the three keys and leaves the row: a landed merge is not retracted by a failed delete (ADR-0065 D7).
case_merge_guards() {
  local name=case_merge_guards response body id id2 base wt wt2 tip main branch2
  # Two statements, not one, for the reason `case_changes` gives: in a single `local` every right-hand side expands
  # before either assignment, so `repo="$projects/holosmith"` would read an unset `projects` under `set -u`.
  local projects="$WORK/projects-guards"
  local data="$WORK/data-guards"
  local repo="$projects/holosmith"
  local db="$data/app.db"
  # The guard comes first, before `stop_serve`, for the reason `case_worktree` gives.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 for the `_ping` socket and the row write (tests/merge.rs covers both guards)\n' "$name"
    return 0
  fi
  stop_serve
  mkdir -p "$repo"
  git -C "$repo" init --quiet -b main || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet --allow-empty \
    -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  base="$(git -C "$repo" rev-parse HEAD)" || fail "$name/git" "git rev-parse did not work in $repo"

  socket_pid="$(fake_docker_ping "$WORK/docker-guards.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker-guards.sock" ] && break
    sleep 0.05
  done
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-guards.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n[merge]\ntargets = ["main"]\n' \
    "$owner" "$projects" "$WORK" >"$WORK/config-guards.toml"
  start_serve "$WORK/config-guards.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-guards-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
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
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_row/base" "the row does not record the branch's base commit: $body"
  # The network step failed on the shim, which is what makes this boot possible at all; the row the merge route
  # reads must therefore not be `idle`. Asserting it here is what makes arm 4 a measurement of `error`.
  printf '%s\n' "$body" | grep -qF '"status":"error"' ||
    fail "${name}_row/status" "the failed provision did not leave the row in error: $body"
  assertions=$((assertions + 1))

  wt="$projects/.worktrees/$id"
  # The step's own command, absolute paths: the failed network step took the worktree back.
  git -C "$repo" worktree add --quiet -b "agent/$id" "$wt" "$base" ||
    fail "$name/worktree" "git worktree add did not work for $wt"
  printf 'one\n' >"$wt/a.txt" || fail "${name}/write" "a.txt is not writable"
  git -C "$wt" add a.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the session's change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  main="$(git -C "$repo" rev-parse refs/heads/main)" || fail "${name}/rev" "git rev-parse did not work"

  # 1. **`dirty_worktree`**: an untracked file in the session's own worktree. Nothing moves, the file is untouched,
  # and the sentence says what did not happen and what to do instead.
  printf 'still being written\n' >"$wt/wip.txt" || fail "${name}/write" "wip.txt is not writable"
  fetch_body "${name}_dirty" POST "/api/sessions/$id/merge" '{"target":"main"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_dirty/no_answer" "POST /api/sessions/$id/merge did not answer"
  response="$(answer "${name}_dirty")"
  expect_status "${name}_dirty" "$response" 409
  expect_code "${name}_dirty" "$response" dirty_worktree
  expect_header "${name}_dirty" "$response" "Content-Type" "application/json"
  body="$(cat "$WORK/responses/${name}_dirty.body")"
  printf '%s\n' "$body" | grep -qF 'nothing was merged' ||
    fail "${name}_dirty/sentence" "the sentence does not say nothing happened: $body"
  assertions=$((assertions + 1))
  # The sentence is what the Changes sheet's offer will render, so it has to name a commit — the affordance
  # adds is a prompt to the agent, and a refusal that named no next step would leave it inventing one.
  # The needle is the offer and not the word "commit": "uncommitted" contains that, so a bare-word guard stays
  # green when the whole next-step half is deleted (S1).
  printf '%s\n' "$body" | grep -qF 'ask the agent to commit' ||
    fail "${name}_dirty/offer" "the sentence names no commit to ask for: $body"
  assertions=$((assertions + 1))
  check "${name}_dirty/ref" "$main" "$(git -C "$repo" rev-parse refs/heads/main)"
  check "${name}_dirty/branch" "$tip" "$(git -C "$repo" rev-parse "agent/$id")"
  check "${name}_dirty/file" "still being written" "$(cat "$wt/wip.txt")"
  if [ -e "$data/merge" ]; then
    fail "${name}_dirty/temporary" "a refused merge made $data/merge"
  fi
  assertions=$((assertions + 1))

  # 2. **An ignored file is not work.** A committed `.gitignore` and a log file: the guard is `changes::dirty`'s,
  # and dropping to `--ignored` or to `ls-files --others` would refuse here.
  printf '*.log\n' >"$wt/.gitignore" || fail "${name}/write" ".gitignore is not writable"
  git -C "$wt" add .gitignore || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "ignore the logs" ||
    fail "${name}/commit" "git commit did not work in $wt"
  printf 'noise\n' >"$wt/build.log" || fail "${name}/write" "build.log is not writable"
  rm -f "$wt/wip.txt" || fail "${name}/rm" "wip.txt is not removable"
  fetch_body "${name}_ignored" POST "/api/sessions/$id/merge" '{"target":"main"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_ignored/no_answer" "the merge of an ignored file did not answer"
  response="$(answer "${name}_ignored")"
  expect_status "${name}_ignored" "$response" 200
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  check "${name}_ignored/ref" "$tip" "$(git -C "$repo" rev-parse refs/heads/main)"

  # 3. **`session_running`**: the row's one column, written by hand, because a gate may not run a turn.
  set_row_status "$db" "$id" running || fail "${name}/row" "the row's status was not writable"
  fetch "${name}_running_row" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
    fail "${name}_running_row/no_answer" "GET /api/sessions/$id did not answer"
  body="$(cat "$WORK/responses/${name}_running_row.body")"
  printf '%s\n' "$body" | grep -qF '"status":"running"' ||
    fail "${name}_running_row/status" "the row is not running: $body"
  assertions=$((assertions + 1))
  fetch_body "${name}_running" POST "/api/sessions/$id/merge" '{"target":"main"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_running/no_answer" "the merge of a running session did not answer"
  response="$(answer "${name}_running")"
  expect_status "${name}_running" "$response" 409
  expect_code "${name}_running" "$response" session_running
  expect_header "${name}_running" "$response" "Content-Type" "application/json"
  body="$(cat "$WORK/responses/${name}_running.body")"
  printf '%s\n' "$body" | grep -qF 'nothing was merged' ||
    fail "${name}_running/sentence" "the sentence does not say nothing happened: $body"
  assertions=$((assertions + 1))
  check "${name}_running/ref" "$tip" "$(git -C "$repo" rev-parse refs/heads/main)"
  if [ -e "$data/merge" ]; then
    fail "${name}_running/temporary" "a refused merge made $data/merge"
  fi
  assertions=$((assertions + 1))

  # 4. **The negative half**: the row back to the `error` its failed provision left, and the same request lands.
  # An `!= Idle` guard — which is how "not running" gets written by mistake — refuses exactly this row.
  set_row_status "$db" "$id" error || fail "${name}/row" "the row's status was not writable"
  printf 'two\n' >"$wt/extra.txt" || fail "${name}/write" "extra.txt is not writable"
  git -C "$wt" add extra.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "one more change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  fetch_body "${name}_error_row" POST "/api/sessions/$id/merge" '{"target":"main"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_error_row/no_answer" "the merge of an error row did not answer"
  response="$(answer "${name}_error_row")"
  expect_status "${name}_error_row" "$response" 200
  check "${name}_error_row/ref" "$tip" "$(git -C "$repo" rev-parse refs/heads/main)"

  # 5. **`delete_session: true` after a squash**: the work lands, the row goes, the worktree goes, and
  # `agent/<id>` goes — a squash leaves the branch a non-ancestor of the target, so this is the rule that story's
  # keep-rule would *not* delete on its own (ADR-0064 D11, ADR-0065 D6).
  printf 'three\n' >"$wt/third.txt" || fail "${name}/write" "third.txt is not writable"
  git -C "$wt" add third.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "a third change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  fetch_body "${name}_squash_delete" POST "/api/sessions/$id/merge" \
    '{"target":"main","strategy":"squash","delete_session":true}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_squash_delete/no_answer" "the squash-and-delete did not answer"
  response="$(answer "${name}_squash_delete")"
  expect_status "${name}_squash_delete" "$response" 200
  expect_header "${name}_squash_delete" "$response" "Content-Type" "application/json"
  body="$(cat "$WORK/responses/${name}_squash_delete.body")"
  main="$(git -C "$repo" rev-parse refs/heads/main)" || fail "${name}/rev" "git rev-parse did not work"
  printf '%s\n' "$body" | grep -qF "\"merged_commit\":\"$main\"" ||
    fail "${name}_squash_delete/commit" "the answer does not name where the target is: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF '"strategy":"squash"' ||
    fail "${name}_squash_delete/strategy" "the answer does not name the strategy that ran: $body"
  assertions=$((assertions + 1))
  # Rule 6's answer, exactly: the delete is not a fourth key.
  case "$body" in
    *'"merged_commit"'*'"target"'*'"strategy"'*'"kept"'*)
      fail "${name}_squash_delete/keys" "the answer gained a key: $body"
      ;;
    *)
      assertions=$((assertions + 1))
      ;;
  esac
  # The squash's commit has the old target as its only parent and the tree of the commit this request landed —
  # the branch's work is in the target even though the branch's own commits are not. `$tip` and not
  # `agent/<id>^{tree}`: the delete this answer started is already removing the worktree and the branch, so
  # reading through either is a race, and a commit object outlives the branch that named it.
  check "${name}_squash_delete/tree" "$(git -C "$repo" rev-parse "$tip^{tree}")" \
    "$(git -C "$repo" rev-parse "refs/heads/main^{tree}")"
  wait_for_gone "${name}_squash_delete_row" "$id"
  if [ -e "$wt" ]; then
    fail "${name}_squash_delete/worktree" "the worktree is still there after the delete"
  fi
  assertions=$((assertions + 1))
  check "${name}_squash_delete/branch" "" "$(git -C "$repo" branch --list 'agent/*')"
  # Rule 2's last sentence, again: a landing leaves no temporary worktree behind, and neither does its delete.
  # The `-d` arm is `case_merge_strategies`' and it is load-bearing: a missing directory makes `find` print
  # nothing, so the count is `0` and this assertion would pass over nothing at all.
  if [ -d "$data/merge" ]; then
    check "${name}/temporary" "0" "$(find "$data/merge" -mindepth 1 | wc -l | tr -d ' ')"
  else
    fail "${name}/temporary" "the merge never made $data/merge, so this case measured nothing"
  fi

  # 6. **The contrast, in a second session**: the same squash *without* the flag keeps the branch — and the
  # ordinary delete afterwards says so in its `kept`, which is the answer unchanged by this story. In between,
  # a `delete_session` whose delete cannot snapshot answers `200` with the three keys and leaves the row: a landed
  # merge is not retracted by a failed delete (ADR-0065 D7).
  fetch_body "${name}_create2" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-guards-2" ||
    fail "${name}_create2/no_answer" "the second create did not answer"
  response="$(answer "${name}_create2")"
  expect_status "${name}_create2" "$response" 202
  body="$(cat "$WORK/responses/${name}_create2.body")"
  id2="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id2" ] || fail "${name}_create2/id" "the second response names no id: $body"
  body=""
  for _ in $(seq 1 100); do
    fetch "${name}_row2" GET "/api/sessions/$id2" "Host: 127.0.0.1" ||
      fail "${name}_row2/no_answer" "GET /api/sessions/$id2 did not answer"
    body="$(cat "$WORK/responses/${name}_row2.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.1 ;;
      *) break ;;
    esac
  done
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$main\"" ||
    fail "${name}_row2/base" "the second row does not record the branch's base commit: $body"
  wt2="$projects/.worktrees/$id2"
  git -C "$repo" worktree add --quiet -b "agent/$id2" "$wt2" "$main" ||
    fail "${name}/worktree2" "git worktree add did not work for $wt2"
  printf 'four\n' >"$wt2/four.txt" || fail "${name}/write" "four.txt is not writable"
  git -C "$wt2" add four.txt || fail "${name}/add" "git add did not work in $wt2"
  git -C "$wt2" -c user.name=verify -c user.email=verify@example commit --quiet -m "the fourth change" ||
    fail "${name}/commit" "git commit did not work in $wt2"
  fetch_body "${name}_squash_keep" POST "/api/sessions/$id2/merge" '{"target":"main","strategy":"squash"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_squash_keep/no_answer" "the keeping squash did not answer"
  response="$(answer "${name}_squash_keep")"
  expect_status "${name}_squash_keep" "$response" 200
  branch2="$(git -C "$repo" rev-parse --verify --quiet "refs/heads/agent/$id2" || true)"
  [ -n "$branch2" ] || fail "${name}_squash_keep/branch" "a squash with no delete_session deleted the branch"
  assertions=$((assertions + 1))
  # ... and the row is still there, which is the other half of "the flag is what deletes".
  fetch "${name}_kept_row" GET "/api/sessions/$id2" "Host: 127.0.0.1" ||
    fail "${name}_kept_row/no_answer" "GET /api/sessions/$id2 did not answer"
  expect_status "${name}_kept_row" "$(answer "${name}_kept_row")" 200

  # 6a. **A delete that cannot snapshot does not retract the merge**: the session's own directory cannot be made,
  # so `snapshot::take` fails and the delete answers `unsaved_work` — which the merge route logs rather than
  # answering, because the work landed. The row is still there afterwards.
  # `snapshot::take` creates `<data>/sessions/<id>` and then its temporary index, so the failure this arm needs is
  # the first write: the parent exists, the session's own directory must not, and the parent refuses creates. The
  # assertion is here because the arm is otherwise indirect — a later story whose boot writes `session.log` for
  # this session would make the chmod a no-op that still passed.
  if [ -e "$data/sessions/$id2" ]; then
    fail "${name}/sessions" "$data/sessions/$id2 exists, so the read-only parent would not fail the snapshot"
  fi
  assertions=$((assertions + 1))
  mkdir -p "$data/sessions" || fail "${name}/sessions" "$data/sessions is not creatable"
  chmod 0555 "$data/sessions" || fail "${name}/chmod" "$data/sessions is not chmoddable"
  fetch_body "${name}_delete_refused" POST "/api/sessions/$id2/merge" \
    '{"target":"main","strategy":"squash","delete_session":true}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_delete_refused/no_answer" "the refused-delete merge did not answer"
  response="$(answer "${name}_delete_refused")"
  expect_status "${name}_delete_refused" "$response" 200
  body="$(cat "$WORK/responses/${name}_delete_refused.body")"
  printf '%s\n' "$body" | grep -qF '"merged_commit"' ||
    fail "${name}_delete_refused/keys" "the landed merge's answer does not name the commit: $body"
  assertions=$((assertions + 1))
  fetch "${name}_after_refused" GET "/api/sessions/$id2" "Host: 127.0.0.1" ||
    fail "${name}_after_refused/no_answer" "GET /api/sessions/$id2 did not answer"
  response="$(answer "${name}_after_refused")"
  expect_status "${name}_after_refused" "$response" 200
  body="$(cat "$WORK/responses/${name}_after_refused.body")"
  case "$body" in
    *'"status":"deleting"'*) fail "${name}_after_refused/status" "the row is deleting after a failed delete: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  chmod 0755 "$data/sessions" || fail "${name}/chmod" "$data/sessions is not chmoddable"

  # 6b. **The ordinary delete keeps the squashed branch**, which is the rule unchanged by this story.
  fetch "${name}_deleted" DELETE "/api/sessions/$id2" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_deleted/no_answer" "DELETE /api/sessions/$id2 did not answer"
  response="$(answer "${name}_deleted")"
  expect_status "${name}_deleted" "$response" 202
  body="$(cat "$WORK/responses/${name}_deleted.body")"
  printf '%s\n' "$body" | grep -qF "\"kept\":{\"branch\":\"agent/$id2\"" ||
    fail "${name}_deleted/kept" "the 202 does not name the branch it keeps: $body"
  assertions=$((assertions + 1))
  wait_for_gone "${name}_deleted_row" "$id2"
  branch2="$(git -C "$repo" rev-parse --verify --quiet "refs/heads/agent/$id2" || true)"
  [ -n "$branch2" ] || fail "${name}_deleted/branch" "the ordinary delete took a branch it said it kept"
  assertions=$((assertions + 1))

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# the proof from the artifact: a real push into a **bare repository under `$WORK`**, and every refusal
# around it. The remote is the story's own constraint made a measurement — it is on the host's filesystem and in
# no container's mount vector, so a `200` and a `git ls-remote` that names the branch is evidence that the
# **host** pushed, not a container and not a stub. `publish.remotes = []` is a second boot, because the disabled
# endpoint is a property of the process's config and not of a request.
#
# Its own projects root **and** its own data root, like the merge cases: two boots over one data root are the
# second arm, and `<data>` is where the session's own directory lives. The `[merge] targets` list is `["trunk"]`
# so the target refusal is reachable and distinct from the protected names — with the default list, `main` is
# protected first and the second fact could not be measured at all.
case_publish() {
  local name=case_publish response body id base wt tip first refs_before pushurl refusals arm
  # Two statements, not one, for the reason `case_changes` gives: in a single `local` every right-hand side
  # expands before either assignment, so `repo="$projects/holosmith"` would read an unset `projects` under
  # `set -u`.
  local projects="$WORK/projects-publish"
  local data="$WORK/data-publish"
  local repo="$projects/holosmith"
  local bare="$WORK/publish-remote.git"
  local config="$WORK/config-publish.toml"
  # The guard comes first, before `stop_serve`, for the reason `case_worktree` gives.
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 for the `_ping` socket (tests/publish.rs covers the push and every refusal)\n' "$name"
    return 0
  fi
  stop_serve
  mkdir -p "$repo" || fail "$name/mkdir" "$repo is not creatable"
  git -C "$repo" init --quiet -b main || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet --allow-empty \
    -m "the first commit" || fail "$name/git" "git commit did not work in $repo"
  base="$(git -C "$repo" rev-parse HEAD)" || fail "$name/git" "git rev-parse did not work in $repo"
  # The remote: a bare repository with no network, no daemon and no credential. `origin` is added by URL, so the
  # push is a real one and the refs git writes are the measurement.
  mkdir -p "$bare" || fail "$name/mkdir" "$bare is not creatable"
  git -C "$bare" init --quiet --bare || fail "$name/git" "git init --bare did not work in $bare"
  git -C "$repo" remote add origin "$bare" || fail "$name/git" "git remote add did not work in $repo"

  socket_pid="$(fake_docker_ping "$WORK/docker-publish.sock")"
  for _ in $(seq 1 100); do
    [ -S "$WORK/docker-publish.sock" ] && break
    sleep 0.05
  done
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-publish.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n[merge]\ntargets = ["trunk"]\n[publish]\nremotes = ["origin"]\n' \
    "$owner" "$projects" "$WORK" >"$config"
  start_serve "$config" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-publish-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
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
  printf '%s\n' "$body" | grep -qF "\"base_commit\":\"$base\"" ||
    fail "${name}_row/base" "the row does not record the branch's base commit: $body"
  assertions=$((assertions + 1))

  wt="$projects/.worktrees/$id"
  # The step's own command, absolute paths: the failed network step took the worktree back.
  git -C "$repo" worktree add --quiet -b "agent/$id" "$wt" "$base" ||
    fail "$name/worktree" "git worktree add did not work for $wt"
  printf 'one\n' >"$wt/a.txt" || fail "${name}/write" "a.txt is not writable"
  git -C "$wt" add a.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "the session's change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  first="$tip"
  # The remote starts with no branch at all, which is what makes the first assertion a creation.
  check "${name}/empty" "" "$(git -C "$bare" for-each-ref --format='%(refname)' refs/heads)"

  # 1. **The push.** Item 12's ordinary request: our branch, under its own name, on the remote. The answer is
  # exactly three keys — `touches_ci` is the changes view's and the warning is that story's, so a flag the route
  # never computed must not appear — and the commit it names is the branch's tip.
  fetch_body "${name}_push" POST "/api/sessions/$id/publish" '{"remote":"origin","confirm":true}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_push/no_answer" "POST /api/sessions/$id/publish did not answer"
  response="$(answer "${name}_push")"
  expect_status "${name}_push" "$response" 200
  expect_header "${name}_push" "$response" "Content-Type" "application/json"
  body="$(cat "$WORK/responses/${name}_push.body")"
  printf '%s\n' "$body" | grep -qF '"remote":"origin"' ||
    fail "${name}_push/remote" "the answer does not name the remote: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF "\"branch\":\"agent/$id\"" ||
    fail "${name}_push/branch" "the answer does not name the destination: $body"
  assertions=$((assertions + 1))
  printf '%s\n' "$body" | grep -qF "\"commit\":\"$tip\"" ||
    fail "${name}_push/commit" "the answer does not name the commit that was read: $body"
  assertions=$((assertions + 1))
  case "$body" in
    *touches_ci*) fail "${name}_push/keys" "the answer carries the changes view's flag: $body" ;;
    *) assertions=$((assertions + 1)) ;;
  esac
  check "${name}_push/ref" "$tip" "$(git -C "$bare" rev-parse "refs/heads/agent/$id")"
  # ... and the local branch did not move: a publish pushes and lands nothing.
  check "${name}_push/local" "$tip" "$(git -C "$repo" rev-parse "agent/$id")"

  # 2. **The destination is the remote's ref.** `branch:"pr/one"` creates *that* ref at the branch's tip and
  # leaves `agent/<id>` where it was; a build that read `branch` as a second local branch would push nothing.
  fetch_body "${name}_destination" POST "/api/sessions/$id/publish" \
    '{"remote":"origin","branch":"pr/one","confirm":true}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_destination/no_answer" "the destination publish did not answer"
  response="$(answer "${name}_destination")"
  expect_status "${name}_destination" "$response" 200
  body="$(cat "$WORK/responses/${name}_destination.body")"
  printf '%s\n' "$body" | grep -qF '"branch":"pr/one"' ||
    fail "${name}_destination/branch" "the answer does not name the destination ref: $body"
  assertions=$((assertions + 1))
  check "${name}_destination/ref" "$tip" "$(git -C "$bare" rev-parse refs/heads/pr/one)"
  if git -C "$repo" rev-parse --verify --quiet refs/heads/pr/one >/dev/null; then
    fail "${name}_destination/local" "the publish made a local branch called pr/one"
  fi
  assertions=$((assertions + 1))
  check "${name}_destination/ours" "$tip" "$(git -C "$bare" rev-parse "refs/heads/agent/$id")"

  # 3. **The request's refusals**, each with the remote's refs read before and after: a refused publish touches
  # nothing. `main` is protected; `trunk` is this machine's merge target (the config's list is `["trunk"]`, which
  # is what makes the second fact reachable at all); `upstream` is not configured; and `confirm:false` is a
  # request that said no.
  refs_before="$(git -C "$bare" for-each-ref --format='%(refname) %(objectname)' refs/heads | sort)"
  for arm in 'protected|{"remote":"origin","branch":"main","confirm":true}' \
    'target|{"remote":"origin","branch":"trunk","confirm":true}' \
    'remote|{"remote":"upstream","confirm":true}' \
    'confirm|{"remote":"origin","confirm":false}'; do
    refusals="${arm%%|*}"
    fetch_body "${name}_$refusals" POST "/api/sessions/$id/publish" "${arm#*|}" \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "${name}_$refusals/no_answer" "the $refusals publish did not answer"
    response="$(answer "${name}_$refusals")"
    expect_status "${name}_$refusals" "$response" 422
    body="$(cat "$WORK/responses/${name}_$refusals.body")"
    printf '%s\n' "$body" | grep -qF 'nothing was pushed' ||
      fail "${name}_$refusals/sentence" "the sentence does not say nothing happened: $body"
    assertions=$((assertions + 1))
  done
  expect_code "${name}_protected" "$(answer "${name}_protected")" protected_branch
  expect_code "${name}_target" "$(answer "${name}_target")" target_not_pushable
  expect_code "${name}_remote" "$(answer "${name}_remote")" bad_remote
  expect_code "${name}_confirm" "$(answer "${name}_confirm")" confirm_required
  check "${name}/refusals" "$refs_before" \
    "$(git -C "$bare" for-each-ref --format='%(refname) %(objectname)' refs/heads | sort)"

  # 4. **The remote's own refusal.** A second commit is published (the remote moves), the branch is then rewound,
  # and the same request is `409 push_rejected` — the remote has a commit this branch does not, which is exactly
  # what a force-push would overwrite. The remote's refs are unchanged afterwards, which is the whole of "never
  # force-pushes".
  printf 'two\n' >"$wt/b.txt" || fail "${name}/write" "b.txt is not writable"
  git -C "$wt" add b.txt || fail "${name}/add" "git add did not work in $wt"
  git -C "$wt" -c user.name=verify -c user.email=verify@example commit --quiet -m "a second change" ||
    fail "${name}/commit" "git commit did not work in $wt"
  tip="$(git -C "$wt" rev-parse HEAD)" || fail "${name}/head" "git rev-parse did not work in $wt"
  fetch_body "${name}_second" POST "/api/sessions/$id/publish" '{"remote":"origin","confirm":true}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_second/no_answer" "the second publish did not answer"
  response="$(answer "${name}_second")"
  expect_status "${name}_second" "$response" 200
  check "${name}_second/ref" "$tip" "$(git -C "$bare" rev-parse "refs/heads/agent/$id")"
  git -C "$wt" reset --hard --quiet "$first" || fail "${name}/reset" "git reset did not work in $wt"
  refs_before="$(git -C "$bare" for-each-ref --format='%(refname) %(objectname)' refs/heads | sort)"
  fetch_body "${name}_moved_on" POST "/api/sessions/$id/publish" '{"remote":"origin","confirm":true}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_moved_on/no_answer" "the rejected publish did not answer"
  response="$(answer "${name}_moved_on")"
  expect_status "${name}_moved_on" "$response" 409
  expect_code "${name}_moved_on" "$response" push_rejected
  body="$(cat "$WORK/responses/${name}_moved_on.body")"
  printf '%s\n' "$body" | grep -qF 'never force-pushes' ||
    fail "${name}_moved_on/sentence" "the sentence does not name the next step: $body"
  assertions=$((assertions + 1))
  check "${name}_moved_on/remote" "$refs_before" \
    "$(git -C "$bare" for-each-ref --format='%(refname) %(objectname)' refs/heads | sort)"
  check "${name}_moved_on/ref" "$tip" "$(git -C "$bare" rev-parse "refs/heads/agent/$id")"

  # 5. **The push is the host's own git, with the host's environment.** The server is booted with git's
  # `GIT_CONFIG_*` pair naming a `remote.origin.pushurl` for a **second** bare repository, so a push that inherits
  # the environment lands there and not at the URL the repository itself names. This is the arm that makes the
  # inheritance a measurement: `.env_clear()` on the push's command is the one change that would silently destroy
  # "the host's own git credentials" while every other arm stayed green (the other pushes go to a local path that
  # needs no credential at all), and under it this ref would appear in `$bare` and not in `$pushurl` — which is
  # exactly the pair asserted here.
  stop_serve
  pushurl="$WORK/publish-pushurl.git"
  mkdir -p "$pushurl" || fail "$name/mkdir" "$pushurl is not creatable"
  git -C "$pushurl" init --quiet --bare || fail "$name/git" "git init --bare did not work in $pushurl"
  refs_before="$(git -C "$bare" for-each-ref --format='%(refname) %(objectname)' refs/heads | sort)"
  GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=remote.origin.pushurl GIT_CONFIG_VALUE_0="$pushurl" \
    start_serve "$config" "$data"
  fetch_body "${name}_environment" POST "/api/sessions/$id/publish" \
    '{"remote":"origin","branch":"env/one","confirm":true}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_environment/no_answer" "the environment publish did not answer"
  response="$(answer "${name}_environment")"
  expect_status "${name}_environment" "$response" 200
  check "${name}_environment/ref" "$first" "$(git -C "$pushurl" rev-parse refs/heads/env/one)"
  check "${name}_environment/not_the_url" "$refs_before" \
    "$(git -C "$bare" for-each-ref --format='%(refname) %(objectname)' refs/heads | sort)"

  # 6. **The disabled endpoint is the process's config**, so it is a third boot over the same data root: the key
  # names no remote, and the answer is `403 publish_disabled` — for the row that exists and for an id the table
  # does not hold, because an endpoint the config turned off answers before it reads a session.
  stop_serve
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\nprojects_root = "%s"\ndocker_host = "unix://%s/docker-publish.sock"\nmin_free_gb = 1\nidle_stop_min = 0\n[merge]\ntargets = ["trunk"]\n[publish]\nremotes = []\n' \
    "$owner" "$projects" "$WORK" >"$config"
  start_serve "$config" "$data"
  for disabled_id in "$id" 0123456789abcdef; do
    fetch_body "${name}_disabled_$disabled_id" POST "/api/sessions/$disabled_id/publish" \
      '{"remote":"origin","confirm":true}' \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "${name}_disabled/no_answer" "the disabled publish did not answer"
    response="$(answer "${name}_disabled_$disabled_id")"
    expect_status "${name}_disabled_$disabled_id" "$response" 403
    expect_code "${name}_disabled_$disabled_id" "$response" publish_disabled
  done
  check "${name}_disabled/remote" "$refs_before" \
    "$(git -C "$bare" for-each-ref --format='%(refname) %(objectname)' refs/heads | sort)"

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
}

# the proof from the artifact: three unsafe repositories, each refused with its own code before a row
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

# the proof from the artifact: a machine that cannot run Docker sessions refuses *at startup*, with
# one sentence that names the fix, and binds nothing.
#
# The case points `docker_host` at a socket that cannot exist, so it is deterministic on every machine —
# including one with a running daemon, which is what a case that waited for "no docker here" would depend
# on. It boots nothing that serves, so it needs no `start_serve`: exit code, stderr, and no banner.
case_docker_preflight() {
  local name=case_docker_preflight status=0
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "docker"\ndocker_host = "unix://%s/absent.sock"\n' \
    "$owner" "$WORK" >"$WORK/config-docker.toml"
  # `--admin-dir` like every other boot in this run: a case must never bind the machine's own
  # `/data/run/drain.sock`, and the refusal this case measures happens before any socket is bound.
  "$artifact" serve --config "$WORK/config-docker.toml" --port 0 --data-dir "$WORK/data-docker" \
    --admin-dir "$WORK/data/run" \
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

# the `--quick`, from the shipped artifact, on a machine that needs no daemon: six checks that are the
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
# the `server deploy`, from the shipped artifact. Two halves that skip independently, because the bugs it
# exists to catch are of two kinds — the framework and the words, which need no daemon, and the install and the
# two proxies on a real one (ADR-0056).
#
# The **offline** half answers `GET /_ping` from a socket it starts, so step 1 passes on a machine with no
# daemon, and asserts four things: the dry run lists the six steps in the spec's order, step 3 planning a build
# of both images, steps 5 and 6 planning what they would do (none of them is a skip, and the
# report has no `incomplete` line), and leaves the install root **absent**; a run whose `docker_host` names a
# socket that cannot exist stops at step 1 and prints the resume command as its last line; a run that gets past
# step 1 and then fails step 3 — that same `_ping` socket answers the ping and nothing a build needs — leaves
# step 2's files on disk, evaluates no later step at all, and prints the same runnable resume command. A dry run
# **reads** step 6 (`tailscale serve status --json`), so this half puts the shim on PATH; that read and the
# `/api/health` wait are the two machine facts a plan cannot be made without.
#
# The **online** half needs the daemon and an image `agent_image` names (today a `docker tag` of the test image,
# which this case does not build — step 3 builds `agent-image:<version>`, and the line says the config's
# `agent_image` names something else): a real install into a root under `target/` — step 2's paths with their
# modes, step 3's two images, step 4's two containers, step 5's unit and step 6's health — and then the same command
# again, where all six steps say `satisfied`, the tree's listing and `config.toml`'s bytes are identical and
# `keys.toml` was not rewritten. That is the acceptance's "the second run changes nothing", as a comparison rather
# than an absence of output. Step 6's answer is `health_responder`'s: this machine has neither a `tailscaled`
# nor a unit boot, and a case that assumed one would be a gate that only passes on a machine that already serves.
#
# Only a failed step exits 1. `tests/deploy.rs` is where the words, the order, the modes and the `sudo` seam are
# driven over shims.

# The config a fresh host would have, written by the case rather than by the deploy: `data_root` is deliberately
# left at its default, because `--root` is what prefixes it and a file that already named the prefixed path would
# be prefixed twice (ADR-0056).
# `role` is the sixth argument and defaults to `server`: the `hub deploy` takes the same machine with its own
# half, and a second config writer would be a second spelling of the eleven fields a deploy reads.
deploy_config() {
  local path="$1" control="$2" preview="$3" docker_host="$4" image="$5" role="${6:-server}"
  cat >"$path" <<EOF
owner = "$owner"
role = "$role"
backend = "stub"
control_listen = "127.0.0.1:$control"
preview_listen = "127.0.0.1:$preview"
control_origin = "http://localhost:$control"
preview_origin = "http://localhost:$preview"
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

# One `tailscale` for the two questions a deploy run asks it: step 6 **reads** `serve status --json` and
# applies each origin with `serve --bg --yes --https=<port> <target>`, and `doctor`'s tailscale finding asks
# `status --json`. One program because both go through the name on PATH, and the state file is the answer a read gets
# back — one `<port> <target>` line per origin — so a second deploy is `satisfied` for the reason a real one would
# be, and a run that applied nothing says `needs`. That read is what a plan is made of, so even a `--dry-run` asks
# it; a machine with no `tailscaled` (this one) cannot answer, which is why every case that reaches step 6 puts this
# directory first on PATH. `tests/deploy.rs` drives both arms of the same seam over a fake `status`.
#
# Every invocation is one line in <log>, `tailscale <all args>`, which is how a case asserts what the deploy asked
# for. The two paths arrive through the environment rather than being written into the script — the shape
# `case_key_proxy_request`'s fake `openssl` uses — so the body stays a plain `sh` script with no quoting of its own.
deploy_machine_shims() {
  local dir="$1" log="$2" state="$3"
  mkdir -p "$dir" || fail "deploy_machine_shims/mkdir" "could not create $dir"
  export HS_SHIM_LOG="$log" HS_SHIM_STATE="$state"
  cat >"$dir/tailscale" <<'SHIM'
#!/bin/sh
# The gate's tailscale: record the call, then answer as the caller expects. Nothing here talks to a daemon.
set -eu
log="$HS_SHIM_LOG"
state="$HS_SHIM_STATE"
{
  printf 'tailscale'
  for arg in "$@"; do
    printf ' %s' "$arg"
  done
  printf '\n'
} >>"$log"
case "${1:-}" in
  serve)
    # The read: one `Web` entry per origin, `Handlers["/"].Proxy` the target — `ipn.ServeConfig`'s shape.
    if [ "${2:-}" = "status" ]; then
      [ "$#" -eq 3 ] || exit 1
      [ "$3" = "--json" ] || exit 1
      entries=""
      if [ -f "$state" ]; then
        while read -r port target; do
          [ -n "$port" ] || continue
          if [ -n "$entries" ]; then
            entries="$entries,"
          fi
          entries="$entries\"holosmith.test.ts.net:$port\":{\"Handlers\":{\"/\":{\"Proxy\":\"$target\"}}}"
        done <"$state"
      fi
      printf '{"Web":{%s}}\n' "$entries"
      exit 0
    fi
    # The apply: `serve --bg --yes --https=<port> <target>`, so the port is $4 and the target $5 — $2 and $3 are
    # the two flags, and a shim that read those as the port would write `--yes --https=8444` into the state and make
    # every run after the first re-apply forever.
    [ "$#" -eq 5 ] || exit 1
    [ "$2" = "--bg" ] || exit 1
    [ "$3" = "--yes" ] || exit 1
    case "$4" in
      --https=*) port="${4#--https=}" ;;
      *) exit 1 ;;
    esac
    target="$5"
    # A temp file and a rename, so a read in the same run never sees half a list.
    : >"$state.new"
    if [ -f "$state" ]; then
      while read -r served_port served_target; do
        [ -n "$served_port" ] || continue
        if [ "$served_port" = "$port" ]; then
          continue
        fi
        printf '%s %s\n' "$served_port" "$served_target" >>"$state.new"
      done <"$state"
    fi
    printf '%s %s\n' "$port" "$target" >>"$state.new"
    mv "$state.new" "$state"
    ;;
  status)
    # `doctor`'s question, which is also what makes step 1's tailscale finding a pass rather than an unknown.
    [ "$#" -eq 2 ] || exit 1
    [ "$2" = "--json" ] || exit 1
    printf '{"BackendState":"Running"}\n'
    ;;
  *)
    exit 1
    ;;
esac
SHIM
  chmod 0755 "$dir/tailscale"
}

# The gate's `ssh`, which is the other half of `case_deploy_ssh`.
#
# It does exactly what ssh does with a command — joins the words after the host and hands them to the target's login
# shell — and adds one thing of its own: `$HOME` points into the case's own tree, so the copy lands where the case can
# read it rather than in this operator's home. The three captured children carry `-o BatchMode=yes` (they have no
# terminal, and one of them has the binary on stdin) and the run child may carry `-t`; ssh's own option grammar is
# `-o` with a value and `-t` without, and a stand-in that skipped only single-word options would take `BatchMode=yes`
# for a host name.
deploy_ssh_shim() {
  local dir="$1"
  cat >"$dir/ssh" <<'SHIM'
#!/bin/sh
# Record every argv first: the log is the only place the four children are measurable.
set -eu
{
  printf 'ssh'
  for arg in "$@"; do
    printf ' %s' "$arg"
  done
  printf '\n'
} >>"$HS_SSH_LOG"
HOME="$HS_SSH_HOME"
export HOME
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o | -p | -l | -i)
      shift
      if [ "$#" -gt 0 ]; then
        shift
      fi
      ;;
    -*)
      shift
      ;;
    *)
      break
      ;;
  esac
done
[ "$#" -gt 0 ] || exit 255
# The host.
shift
[ "$#" -gt 0 ] || exit 255
# The run itself is `sudo -n -u holosmith <binary> server deploy …` (ADR-0086 D2: the deploy that writes a root is
# the service user's, so the remote run is one `sudo -u` and not the ssh login). A gate has no service user, no
# sudoers line and no password to give, so the stand-in drops that prefix and runs the words after it — which are the
# same words this case asserts on.
if [ "$#" -ge 4 ] && [ "$1" = "sudo" ] && [ "$2" = "-n" ] && [ "$3" = "-u" ]; then
  shift 4
fi
exec sh -c "$*"
SHIM
  chmod 0755 "$dir/ssh"
}

# One background `python3` answering `200` to every `GET` on 127.0.0.1:<port> for the rest of the run, printing the
# pid it left behind.
#
# Two callers ask one question and both need a *listener*: `server deploy`'s step 6 waits for `GET /api/health`
# before it calls a start successful, and `doctor`'s port check reads "the port is held and `/api/health` answers" as
# the serving pass. A responder is all this machine can offer there — there is no systemd and no unit boot — and its
# body **is** the health payload: `http::health::ask` parses it, so a bare `200` reads as "a port
# somebody else holds" and would make step 6's wait fail. The stricter answer is also the true one — this is what
# `serve` really answers.
health_responder() {
  python3 - "$1" <<'PY' >/dev/null 2>&1 &
import socket, sys

body = b'{"name":"holosmith","version":"verify","role":"server","uptime_s":0,"preview_origin":"http://localhost:8081"}'
server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", int(sys.argv[1])))
server.listen(16)
while True:
    connection, _ = server.accept()
    connection.recv(4096)
    connection.sendall(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n"
        % len(body)
        + body
    )
    connection.close()
PY
  echo $!
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
  # `install -d -m 0755` rather than `mkdir -p`: `doctor`'s `perms` compares the data root against 0755 because that
  # is what `server deploy` writes, and this shell's umask makes `mkdir -p` write 0775 — the fixture has to be the
  # machine the deploy leaves, not this shell's umask.
  install -d -m 0755 "$root/data" || fail "$name/mkdir" "could not create $root/data"
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
  "$artifact" doctor --quick --config "$WORK/config-doctor-quick.toml" --root "$root" \
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
  "$artifact" doctor --quick --config "$WORK/config-doctor-quick.toml" --root "$root" \
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
  "$artifact" doctor --quick --config "$WORK/config-doctor-absent.toml" --root "$root" \
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

# the floor on the two request paths that *start* work: item 14's "session creation and resume fail with 507
# below `min_free_gb`".
#
# The boot already refuses below the floor, so this case cannot simply be run on a small disk — a server
# that would refuse to start has no request path to measure. The stand-in is therefore a `df` on `PATH` that
# `cat`s a file this case rewrites (`deploy_machine_shims`' shape): the boot measures a healthy machine, one
# session is created on it, and then the answer is lowered so the floor can fire **while the server is up**.
#
# Three facts, and the third is the acceptance's "`doctor` reports the same measurement, so the two never
# disagree": the create answers 507 with the envelope's code and a sentence naming both numbers, a resume of a
# row that already exists answers the same, and `doctor --quick` over the *same* stand-in fails with the same two
# numbers. The row is created before the floor is crossed and is never touched by either refusal, which is item
# 14's fourth line — "only new starts are refused" — as far as a gate may measure it (`tests/disk.rs` measures
# a turn that settles mid-flight; this case only asserts the row is where it was).
#
# `backend = "docker"` for the predicate itself: `disk::guarded` asks the *run's* backend, and a stub run is not
# measured at all. The daemon is `fake_docker_ping`, so nothing here can start a container.
#
# It takes back what it made — its scratch root, its stand-in, its socket — and puts the shared boot back, like
# every other case that reboots the fixture.
case_disk_floor() {
  local name=case_disk_floor root="$WORK/disk-floor" body id response status line saved_path
  local healthy=500 short=5
  # The guard comes first and before `stop_serve`: a skipped case that stopped the server leaves the phase's own
  # teardown with no pid to signal (`case_changes`' note).
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to answer the docker preflight (tests/disk.rs covers both paths)\n' "$name"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$root/bin" "$root/data" "$root/projects/holosmith"
  # The stand-in: the two lines `df -P` promises, written by this case and read by the artifact's own reader.
  # Everything on `PATH` except `df` stays the machine's, which is what keeps the boot's other checks real.
  printf '%s\n' '#!/bin/sh' "cat $root/df-answer" >"$root/bin/df" ||
    fail "$name/stand_in" "could not write the df stand-in"
  chmod 0755 "$root/bin/df"
  disk_floor_answer "$root/df-answer" "$healthy"

  # A repository for the one create: `refuse_project` refuses a project that is not a plain checkout before any
  # row is written, and this case is about the floor rather than about that refusal.
  git -C "$root/projects/holosmith" init --quiet -b main || fail "$name/git" "git init did not work"
  git -C "$root/projects/holosmith" -c user.name=verify -c user.email=verify@example \
    commit --quiet --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work"

  socket_pid="$(fake_docker_ping "$root/docker.sock")"
  for _ in $(seq 1 100); do
    [ -S "$root/docker.sock" ] && break
    sleep 0.05
  done
  [ -S "$root/docker.sock" ] || fail "$name/socket" "the fake daemon socket never appeared"

  # `min_free_gb` at its default rather than a case-specific number: the story's own "default 20", and the
  # sentence has to name it.
  printf 'owner = "%s"\nmax_live_sessions = 3\nbackend = "docker"\nprojects_root = "%s"\ndata_root = "%s"\ndocker_host = "unix://%s/docker.sock"\nmin_free_gb = 20\nidle_stop_min = 0\n' \
    "$owner" "$root/projects" "$root/data" "$root" >"$WORK/config-disk-floor.toml"

  saved_path="$PATH"
  export PATH="$root/bin:$PATH"
  start_serve "$WORK/config-disk-floor.toml" "$root/data"

  # 1. A create on a healthy machine, so there is a row for the resume below to be refused.
  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-disk-floor-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  # The row has to be out of `starting` before a resume may be asked for: the network step fails against the
  # `_ping` shim, which is `case_changes`' boot and leaves the row `error` — a state the resume accepts.
  body=""
  for _ in $(seq 1 200); do
    fetch "${name}_row" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "${name}_row/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/${name}_row.body")"
    case "$body" in
      *'"status":"starting"'*) sleep 0.05 ;;
      *) break ;;
    esac
  done
  expect_status "${name}_row" "$(answer "${name}_row")" 200
  printf '%s\n' "$body" | grep -q '"status":"error"' ||
    fail "${name}_row/state" "the row did not reach a resumable state: $body"
  assertions=$((assertions + 1))

  # 2. The floor is crossed under the running server, and only a new start is refused.
  disk_floor_answer "$root/df-answer" "$short"
  fetch_body "${name}_floor" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-disk-floor-2" ||
    fail "${name}_floor/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_floor")"
  expect_status "${name}_floor" "$response" 507
  expect_code "${name}_floor" "$response" disk_floor
  expect_header "${name}_floor" "$response" content-type "application/json"
  body="$(cat "$WORK/responses/${name}_floor.body")"
  # The sentence is the request's own: it names the free space, the floor and the machine, so an operator on a
  # phone knows how much to free. A sentence that named neither number would pass a status-only assertion.
  for line in "has $short GB free" "min_free_gb is 20" "$root/data" 'free some space'; do
    printf '%s\n' "$body" | grep -qF "$line" ||
      fail "${name}_floor/sentence" "the refusal does not name \"$line\": $body"
    assertions=$((assertions + 1))
  done

  # 3. The resume of the row that already exists is refused the same way, and the row is untouched: a refused
  # resume must not strand it in `starting`.
  fetch "${name}_resume" POST "/api/sessions/$id/resume" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_resume/no_answer" "POST /api/sessions/$id/resume did not answer"
  response="$(answer "${name}_resume")"
  expect_status "${name}_resume" "$response" 507
  expect_code "${name}_resume" "$response" disk_floor
  fetch "${name}_after" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
    fail "${name}_after/no_answer" "GET /api/sessions/$id did not answer"
  body="$(cat "$WORK/responses/${name}_after.body")"
  printf '%s\n' "$body" | grep -q '"status":"error"' ||
    fail "${name}_after/state" "the refused resume moved the row: $body"
  assertions=$((assertions + 1))

  # 4. Everything else still answers, which is what "only new starts are refused" means from outside.
  fetch "${name}_list" GET /api/sessions "Host: 127.0.0.1" ||
    fail "${name}_list/no_answer" "GET /api/sessions did not answer"
  response="$(answer "${name}_list")"
  expect_status "${name}_list" "$response" 200
  body="$(cat "$WORK/responses/${name}_list.body")"
  printf '%s\n' "$body" | grep -qF "\"id\":\"$id\"" ||
    fail "${name}_list/missing" "the session list lost the row: $body"
  assertions=$((assertions + 1))
  # ...and no route other than the two that start work answers the floor's code.
  if grep -q disk_floor "$WORK/responses/${name}_list.http" "$WORK/responses/${name}_after.http"; then
    fail "${name}_list/floor" "a read answered the floor's code"
  fi
  assertions=$((assertions + 1))

  # 5. `doctor --quick` over the same stand-in: the same measurement, so the two cannot disagree.
  status=0
  "$artifact" doctor --quick --config "$WORK/config-disk-floor.toml" --root "$root" \
    >"$WORK/$name.doctor.out" 2>&1 || status=$?
  check "$name/doctor/exit" 1 "$status"
  line="$(grep '^disk: fail: ' "$WORK/$name.doctor.out" || true)"
  [ -n "$line" ] || fail "$name/doctor/line" "the floor is not a failure in the report: $(cat "$WORK/$name.doctor.out")"
  case "$line" in
    *"has $short GB free"*"min_free_gb is 20"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/doctor/sentence" "doctor does not name the numbers the refusal did: $line" ;;
  esac

  export PATH="$saved_path"
  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
  rm -rf "$root"
  # The shared boot comes back: the cases after this one talk to `$port` and to `$control_origin`, and a case
  # that stopped it and left `serve_pid` empty would make the next unguarded `stop_serve` a hard failure
  # (`case_delete_session`'s idiom, and `case_status` says why).
  stop_serve
  start_serve
}

# The two lines a POSIX `df -Pk` prints, with `gb` gigabytes free. `%%` because it is a printf format.
disk_floor_answer() {
  printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/mapper/root 1000000000 1 %s 1%% /\n' \
    "$(( $2 * 1024 * 1024 ))" >"$1"
}

# the per-session disk soft cap (spec item 14). One pass, over the shipped artifact, with the size a
# stand-in reports.
#
# The policy's interval is the spec's five minutes, so this case cannot wait for a tick — and `session_disk_soft_gb`
# is not the interval. What it does instead is let the watchdog's **first** pass do it: the pass after a restart
# waits for the boot's own re-provisioning to settle (ADR-0069 D6) and then measures, which is two seconds after a
# second `serve` rather than five minutes after the first. So the shape is: a real session, idle, then a restart.
#
# Five measurements:
#
#   measured  the `du` stand-in logged its arguments and they name **that session's** `.worktrees/<id>` — not the
#             data root and not the projects root. This is the story's "a watchdog runs `du` on each live session's
#             worktree", and the only way a gate can see it: no worktree a gate can build is 10 GB, and a case
#             that set a small cap to avoid the stand-in would be measuring the size of a checkout.
#   stopped   the row is `stopped` with `status_reason: "disk_quota"` — the third acceptance bullet, which is the
#             row and not the event ("not `error`").
#   reported  the transition carries the reason on the stream, and the `error` event with `code: "disk_quota"` is
#             in the durable log both ways — the raw payload on the stream and the `type: "error"` frame in a page
#             of `GET …/events/history`.
#   resumed   `POST …/resume` answers 202 and the row walks back to `idle`: "resumable after cleanup", measured. The
#             shim still reports 15 GB, and the session is not stopped again inside this case because the next pass
#             is five minutes away — which is the interval doing its job, not a hole.
#
# It needs the daemon **and** an `agent_image`, because a worktree is the Docker backend's (a stub session has no
# worktree column at all, ADR-0015) and the row has to reach `idle` for the sweep to see it. `tests/quota.rs`
# covers both ladder rungs, the under-the-cap arm, an unmeasurable worktree, `session_disk_soft_gb = 0` and the
# arithmetic without a daemon, so a `SKIP` here is not the bug this case exists to catch.
case_disk_quota() {
  local name=case_disk_quota root="$repo_root/target/verify-quota-$$" image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local repo="$root/projects/holosmith" data="$root/data" saved_path
  local response body id worktree
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/quota.rs covers both arms without one)\n' "$name"
    return 0
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s: no image %s (HOLOSMITH_TEST_IMAGE names another)\n' "$name" "$image"
    return 0
  fi
  stop_serve
  rm -rf "$root"
  mkdir -p "$root/bin" "$repo" || fail "$name/mkdir" "could not create $root"
  git -C "$repo" init --quiet -b main || fail "$name/git" "git init did not work in $repo"
  git -C "$repo" -c user.name=verify -c user.email=verify@example commit --quiet \
    --allow-empty -m "the first commit" || fail "$name/git" "git commit did not work"

  # The stand-in: `du -sk -- <path>` answered with 15 GiB, and every question recorded. Everything on `PATH` except
  # `du` stays the machine's, which is what keeps the boot's other checks and git's work real.
  {
    printf '#!/bin/sh\n'
    printf 'printf "%%s\\n" "$*" >> "%s"\n' "$root/du.log"
    printf 'printf "15728640\\t%%s\\n" "$3"\n'
  } >"$root/bin/du" || fail "$name/stand_in" "could not write the du stand-in"
  chmod 0755 "$root/bin/du"

  # `session_disk_soft_gb` at its default (10) rather than a case-specific number: the story's own number, and one
  # the shim's 15 GiB is over. `min_free_gb = 1` because the gate's `/tmp` is small and this case is not about the
  # floor.
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "docker"\nprojects_root = "%s"\ndata_root = "%s"\nagent_image = "%s"\nmin_free_gb = 1\nidle_stop_min = 0\nturn_timeout_min = 0\nsession_disk_soft_gb = 10\n' \
    "$owner" "$root/projects" "$data" "$image" >"$WORK/config-disk-quota.toml"

  saved_path="$PATH"
  export PATH="$root/bin:$PATH"
  start_serve "$WORK/config-disk-quota.toml" "$data"

  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-disk-quota-1" ||
    fail "${name}_create/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_create")"
  expect_status "${name}_create" "$response" 202
  body="$(cat "$WORK/responses/${name}_create.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "${name}_create/id" "the response names no id: $body"
  docker_session_id="$id"
  wait_for_idle_docker "${name}_create" "$id"

  # The worktree the row recorded: the shim's arguments have to name this, not a path of the case's choosing.
  fetch "${name}_row" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
    fail "${name}_row/no_answer" "GET /api/sessions/$id did not answer"
  body="$(cat "$WORK/responses/${name}_row.body")"
  worktree="$(printf '%s\n' "$body" | sed -n 's/.*"worktree":"\([^"]*\)".*/\1/p')"
  [ -n "$worktree" ] || fail "${name}_row/worktree" "the row names no worktree: $body"

  # The restart: the reconcile re-provisions the row and the watchdog's first pass waits for that and measures.
  stop_serve
  start_serve "$WORK/config-disk-quota.toml" "$data"
  wait_for_stopped_id "${name}_stopped" "$id"
  body="$(cat "$WORK/responses/${name}_stopped.body")"
  case "$body" in
    *'"status":"stopped"'*'"status_reason":"disk_quota"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_stopped/reason" "the stopped row does not say why: $body"
      ;;
  esac

  # 1. The measurement: that session's own worktree, and not the data root.
  if grep -qF -- "-- $worktree" "$root/du.log" 2>/dev/null; then
    assertions=$((assertions + 1))
  else
    fail "${name}/measured" \
      "the watchdog did not measure $worktree: $(cat "$root/du.log" 2>/dev/null | sed -n '1,5p')"
  fi
  if grep -qF -- "-- $data" "$root/du.log" 2>/dev/null; then
    fail "${name}/data_root" "the watchdog measured the data root rather than the worktree: $(cat "$root/du.log")"
  fi
  assertions=$((assertions + 1))

  # 2. The transition, on the stream a client watches, with the reason the policy's own word.
  fetch_sse_until "${name}_stream" "/api/sessions/$id/events" '"reason":"disk_quota"' "Host: 127.0.0.1" ||
    fail "${name}_stream/no_answer" "GET /api/sessions/$id/events did not answer"
  response="$(answer "${name}_stream")"
  expect_status "${name}_stream" "$response" 200
  expect_header "${name}_stream" "$response" content-type "text/event-stream"
  body="$(cat "$WORK/responses/${name}_stream.body")"
  case "$body" in
    *'"from":"idle","to":"stopped","step":null,"reason":"disk_quota"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_stream/transition" "the cap's transition is not in the transcript: $body"
      ;;
  esac

  # 3. The event: `error`, with the code, the measured size and the cap — the story's second bullet.
  fetch_sse_until "${name}_event" "/api/sessions/$id/events" '"code":"disk_quota"' "Host: 127.0.0.1" ||
    fail "${name}_event/no_answer" "GET /api/sessions/$id/events did not answer"
  body="$(cat "$WORK/responses/${name}_event.body")"
  # On the stream the kind is the SSE frame's own `event:` line and the payload is the `data:` line, so the four
  # needles are the frame's shape rather than one JSON object: `type` is a page's field, not the frame's.
  for needle in 'event: error' '"code":"disk_quota"' '"size_kib":15728640' '"limit_gb":10'; do
    printf '%s\n' "$body" | grep -qF -- "$needle" ||
      fail "${name}_event/payload" "the event does not carry $needle: $body"
    assertions=$((assertions + 1))
  done
  # …and the one log line per stop an operator reading journald sees (the idle stop's own fourth bullet).
  grep -q "holosmith::quota: the session's worktree is over the disk cap and was stopped session=$id" \
    "$serve_err" ||
    fail "${name}/log" \
      "the watchdog wrote no line for $id: $(grep 'holosmith::quota' "$serve_err" | sed -n '1,5p')"
  assertions=$((assertions + 1))

  # 4. The same frame in a page of history, which is the durable half: the kind is the page's own field.
  fetch "${name}_history" GET "/api/sessions/$id/events/history?before=1000000" "Host: 127.0.0.1" ||
    fail "${name}_history/no_answer" "GET /api/sessions/$id/events/history did not answer"
  response="$(answer "${name}_history")"
  expect_status "${name}_history" "$response" 200
  body="$(cat "$WORK/responses/${name}_history.body")"
  # Only the kind: the page's `payload` is an escaped string, so the code and the numbers are asserted on the
  # stream above, where they are not escaped. (A `"status"` needle would also be a lie here — an event's payload
  # has `from`/`to`/`step`/`reason`, and `tool.end`'s has its own `status`.)
  printf '%s\n' "$body" | grep -qF '"type":"error"' ||
    fail "${name}_history/kind" "history does not carry the cap's event: $body"
  assertions=$((assertions + 1))

  # 5. Resumable after cleanup: the operator frees space and asks for the session back.
  fetch "${name}_resume" POST "/api/sessions/$id/resume" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_resume/no_answer" "POST /api/sessions/$id/resume did not answer"
  response="$(answer "${name}_resume")"
  expect_status "${name}_resume" "$response" 202
  expect_header "${name}_resume" "$response" content-type "application/json"
  wait_for_idle_docker "${name}_resume" "$id"

  # The machine as it was found: this case's container and its scratch root go, and the shared boot comes back
  # (`case_disk_floor`'s idiom — a case that stopped it and left `serve_pid` empty fails the *next* `stop_serve`).
  docker rm --force "s_$id" >/dev/null 2>&1 || true
  docker network disconnect --force "agents_$id" hs_egress >/dev/null 2>&1 || true
  docker network rm "agents_$id" >/dev/null 2>&1 || true
  docker_session_id=""
  export PATH="$saved_path"
  rm -rf "$root"
  stop_serve
  start_serve
}

# the step 3: the two images, built from the Dockerfiles embedded in the artifact, and what is inside them.
#
# This is the one case whose subject is the *build*: `holosmith-proxy:<version>` and `agent-image:<version>` are what
# step 4's proxies run and what a session's container runs, so both the labels a later deploy compares and the
# images' own contents are measured here, against a real daemon. It owns the **absent** state — it removes both
# tags first — because a build that only ever ran on a machine that already had the images would prove nothing.
#
# Four measurements, one per claim:
#
#   labels    both tags carry version, git tag and pinned pi; the agent image carries the toolchain list as well.
#             That is the read step 3's skip rule and `doctor` make (ADR-0057).
#   agent     inside the image: `pi --version` is the pin, git answers, both toolchains are installed, the uid is
#             1000 (ADR-0035), and this binary runs `container snapshot --help` — the helper subcommand a session
#             needs and the one thing a hand-built `pi-agent:local` does not have.
#   proxy     the proxy image serves *both* proxies: `hs_egress` refuses a host that is not on its allow-list and
#             `hs_key` refuses a provider path it does not know. Neither question needs TLS or the internet.
#   again     the same deploy again says `satisfied` and leaves both image ids unchanged — the acceptance's "a
#             second run changes nothing", for the step that is the most expensive to get wrong.
#
# It **leaves both images** and removes only its own containers and its scratch root: every later case's `serve`
# boot starts the proxies from them, and removing an image a running container came from would leave that container
# unable to restart. `case_server_deploy`'s online half removes the tags again because *it* asserts `changed`; the
# order in `serve_cases` is what makes both honest.
#
# A machine with no daemon or no python3 prints one SKIP line: `tests/images.rs` builds a test tag and asserts the
# same three things through the production `Images` path, so a skipped gate is not a silent gap.
case_server_images() {
  local name=case_server_images root="$repo_root/target/verify-images-$$"
  local proxy agent version tag pin plain status labels id_before control preview line answer
  local egress_ip key_ip binds container package shims log state
  if ! command -v docker >/dev/null 2>&1 || ! docker version >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers (tests/images.rs builds a test tag and reads it back)\n' "$name"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to speak HTTP to the proxies in the image (tests/images.rs covers the build)\n' "$name"
    return 0
  fi
  plain="$("$artifact" version 2>/dev/null)" || fail "$name/version" "the artifact does not report a version"
  version="$(sed -n 's/^version //p' <<<"$plain")"
  tag="$(sed -n 's/^git_tag //p' <<<"$plain")"
  pin="$(tr -d '[:space:]' <"$repo_root/pins/pi.txt")"
  # The fifth label's value: the artifact's own bytes. `sha256sum` is what an operator would run to check it.
  sha="$(sha256sum "$artifact" | cut -d' ' -f1)"
  [ -n "$version" ] && [ -n "$tag" ] && [ -n "$pin" ] && [ -n "$sha" ] ||
    fail "$name/version" "the artifact's version report is incomplete: $plain"
  proxy="holosmith-proxy:$version"
  agent="agent-image:$version"

  # This case owns the absent state: the deploy's own proxy containers go first (a running container pins the image
  # it was started from, so an untag with them up fails and the build that follows would be a no-op), then both tags
  # are removed — and the removal is *asserted*, because "the image was already there" is exactly the state this
  # case exists to rule out.
  timeout 120 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  timeout 120 docker image rm -f "$proxy" "$agent" >/dev/null 2>&1 || true
  for image in "$proxy" "$agent"; do
    if docker image inspect "$image" >/dev/null 2>&1; then
      fail "$name/absent" "$image survived its removal, so the build below would not be measured"
    fi
    assertions=$((assertions + 1))
  done
  rm -rf "$root"
  mkdir -p "$root"
  printf '[providers]\nopencode-go = "sk-opencode-go-verify"\n\n[sessions]\nverify-session = "hs-dummy-verify"\n' \
    >"$root/keys.toml" || fail "$name/keys" "could not write the key file the key proxy mounts"
  chmod 600 "$root/keys.toml"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  # `deploy_config`'s shape with one difference: `agent_image` names the image this step is about to build, so a
  # session's container would run what the deploy installed. `data_root` and `projects_root` stay at their defaults
  # because `--root` prefixes them, and this case's `--root` is under `target/` (ADR-0035's mount-source rule).
  images_config "$WORK/config-images.toml" "$control" "$preview" "$version"
  # Steps 5, 6 and 7 belong to a later change and are real now, so a `--root` deploy reaches them: step 6 **reads**
  # `tailscale serve status --json`, and step 6 waits for `/api/health` and then runs `doctor --full`. This machine
  # has no `tailscaled` and nothing serving, so the two stand-ins answer instead — what this case asserts is the
  # deploy's own reads and the images they leave, not a daemon that happens to be up.
  shims="$WORK/$name.shims"
  log="$WORK/$name.tailscale.log"
  state="$WORK/$name.tailscale.state"
  rm -f "$log" "$state"
  deploy_machine_shims "$shims" "$log" "$state"
  holder_pid="$(health_responder "$control")"
  for _ in $(seq 1 100); do
    if (exec 3<>/dev/tcp/127.0.0.1/"$control") 2>/dev/null; then break; fi
    sleep 0.05
  done

  status=0
  PATH="$shims:$PATH" "$artifact" server deploy --root "$root/install" --config "$WORK/config-images.toml" \
    >"$WORK/$name.deploy.out" 2>&1 || status=$?
  [ "$status" -eq 0 ] ||
    fail "$name/deploy/exit" "the deploy exited $status: $(tail -20 "$WORK/$name.deploy.out")"
  assertions=$((assertions + 1))
  grep -q '^3\. images: changed: ' "$WORK/$name.deploy.out" ||
    fail "$name/deploy/step3" "step 3 did not build the two images: $(cat "$WORK/$name.deploy.out")"
  assertions=$((assertions + 1))

  # The labels, on both tags: the three facts this binary knows about itself.
  for label in version git-tag pi-version binary-sha256; do
    case "$label" in
      version) wanted="$version" ;;
      git-tag) wanted="$tag" ;;
      pi-version) wanted="$pin" ;;
      binary-sha256) wanted="$sha" ;;
    esac
    for image in "$proxy" "$agent"; do
      labels="$(docker image inspect --format '{{json .Config.Labels}}' "$image" 2>/dev/null)" ||
        fail "$name/labels" "$image is not on this machine after the build"
      case "$labels" in
        *"\"org.holosmith.$label\":\"$wanted\""*) assertions=$((assertions + 1)) ;;
        *) fail "$name/labels/$label" "$image does not carry org.holosmith.$label=$wanted: $labels" ;;
      esac
    done
  done
  # The fourth label, and only on the agent image: the toolchain list the config gave it.
  labels="$(docker image inspect --format '{{json .Config.Labels}}' "$agent" 2>/dev/null)"
  case "$labels" in
    *'"org.holosmith.toolchains":"python3 ripgrep"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/labels/toolchains" "the agent image does not carry its toolchain list: $labels" ;;
  esac
  labels="$(docker image inspect --format '{{json .Config.Labels}}' "$proxy" 2>/dev/null)"
  case "$labels" in
    *'"org.holosmith.toolchains"'*)
      fail "$name/labels/proxy-toolchains" "the proxy image carries the agent image's toolchain label: $labels"
      ;;
  esac
  assertions=$((assertions + 1))

  # The agent image's contents, each one a fact a session depends on.
  answer="$(timeout 300 docker run --rm --network none --entrypoint pi "$agent" --version 2>&1)" ||
    fail "$name/agent/pi" '`pi --version` did not run in the agent image: '"$answer"
  check "$name/agent/pi/version" "$pin" "$(sed -n '1s/^[^0-9]*//p' <<<"$answer" | tr -d '[:space:]')"
  timeout 120 docker run --rm --network none --entrypoint git "$agent" --version >/dev/null 2>&1 ||
    fail "$name/agent/git" "git does not run in the agent image"
  assertions=$((assertions + 1))
  for package in python3 ripgrep; do
    answer="$(timeout 120 docker run --rm --network none --entrypoint dpkg-query "$agent" \
      -W -f='${Package}' "$package" 2>&1)" ||
      fail "$name/agent/$package" "$package is not installed in the agent image: $answer"
    check "$name/agent/$package/name" "$package" "$answer"
  done
  # The image's own user is the *invoking* user's uid — that is ADR-0035's rule (a worktree the host can commit to),
  # and it is not a constant: 1000 is the base's `node`, which happens to be this sandbox's uid. A non-root guard is
  # what the invariant needs on top, because uid 0 would pass the equality on a root-owned machine.
  invoking_uid="$(id -u)"
  image_uid="$(timeout 120 docker run --rm --network none --entrypoint id "$agent" -u 2>&1)"
  check "$name/agent/uid" "$invoking_uid" "$image_uid"
  [ "$image_uid" != "0" ] || fail "$name/agent/root" "the agent image runs as root, so a session's files are root's"
  assertions=$((assertions + 1))
  timeout 120 docker run --rm --network none --entrypoint /usr/local/bin/holosmith \
    "$agent" container snapshot --help >/dev/null 2>&1 ||
    fail "$name/agent/snapshot" 'this binary: `container snapshot --help` does not run in the agent image'
  assertions=$((assertions + 1))

  # The proxy image serves both proxies, from the one entrypoint and with nothing of the host's mounted.
  docker network inspect egress >/dev/null 2>&1 ||
    fail "$name/network" "the deploy's step 4 did not leave the egress network the proxies attach to"
  assertions=$((assertions + 1))
  timeout 60 docker rm -f "hs_images_egress_$$" "hs_images_key_$$" >/dev/null 2>&1 || true
  timeout 60 docker run -d --name "hs_images_egress_$$" --network egress \
    --entrypoint /usr/local/bin/holosmith "$proxy" \
    proxy egress --listen 0.0.0.0:3128 --allow example.com >/dev/null 2>&1 ||
    fail "$name/egress/run" "the proxy image did not start an egress proxy"
  assertions=$((assertions + 1))
  timeout 60 docker run -d --name "hs_images_key_$$" --network egress \
    -v "$root/keys.toml:$root/keys.toml:ro" \
    --entrypoint /usr/local/bin/holosmith "$proxy" \
    proxy key --listen 0.0.0.0:8080 --keys "$root/keys.toml" >/dev/null 2>&1 ||
    fail "$name/key/run" "the proxy image did not start a key proxy"
  assertions=$((assertions + 1))
  egress_ip="$(docker inspect --format '{{(index .NetworkSettings.Networks "egress").IPAddress}}' "hs_images_egress_$$" 2>/dev/null)"
  key_ip="$(docker inspect --format '{{(index .NetworkSettings.Networks "egress").IPAddress}}' "hs_images_key_$$" 2>/dev/null)"
  [ -n "$egress_ip" ] && [ -n "$key_ip" ] ||
    fail "$name/address" "a proxy container has no address on the egress network"
  assertions=$((assertions + 1))
  # A host that is not on the allow-list: the refusal spec item 2 names, spoken to the container.
  check "$name/egress/refusal" "HTTP/1.1 403 Forbidden" \
    "$(image_proxy_ask "$egress_ip" 3128 "CONNECT not-listed.example:443 HTTP/1.1" \
      "Proxy-Authorization: Basic $(printf 'abcdef0123456789:pw' | base64)")"
  # And the key proxy's own refusal for a provider path this build does not forward to. No TLS, no provider.
  check "$name/key/refusal" "HTTP/1.1 403 Forbidden" \
    "$(image_proxy_ask "$key_ip" 8080 "POST /not-a-provider/v1/messages HTTP/1.1" "")"
  # Nothing of the host's binary is mounted into the egress proxy: the binary it runs is the image's own copy.
  binds="$(docker inspect --format '{{json .HostConfig.Binds}}' "hs_images_egress_$$" 2>/dev/null)"
  case "$binds" in
    null | "[]") assertions=$((assertions + 1)) ;;
    *) fail "$name/egress/binds" "the egress proxy binds the host's paths: $binds" ;;
  esac
  for container in "hs_images_egress_$$" "hs_images_key_$$"; do
    binds="$(docker inspect --format '{{json .HostConfig.Binds}}' "$container" 2>/dev/null)"
    # The artifact itself, not the word "holosmith": the key proxy's own mount lives under this repository's
    # `target/`, and a check for a substring would refuse the one mount the design keeps.
    case "$binds" in
      *"$artifact"*) fail "$name/binds" "$container mounts the host's binary ($artifact): $binds" ;;
      *) assertions=$((assertions + 1)) ;;
    esac
  done

  # The second deploy: `satisfied`, and neither image was rebuilt.
  id_before="$(docker image inspect --format '{{.Id}}' "$proxy") $(docker image inspect --format '{{.Id}}' "$agent")"
  status=0
  PATH="$shims:$PATH" "$artifact" server deploy --root "$root/install" --config "$WORK/config-images.toml" \
    >"$WORK/$name.again.out" 2>&1 || status=$?
  [ "$status" -eq 0 ] ||
    fail "$name/again/exit" "the second deploy exited $status: $(tail -20 "$WORK/$name.again.out")"
  assertions=$((assertions + 1))
  grep -q '^3\. images: satisfied: ' "$WORK/$name.again.out" ||
    fail "$name/again/step3" "the second run's step 3 is not satisfied: $(cat "$WORK/$name.again.out")"
  assertions=$((assertions + 1))
  check "$name/again/ids" "$id_before" \
    "$(docker image inspect --format '{{.Id}}' "$proxy") $(docker image inspect --format '{{.Id}}' "$agent")"

  # `doctor --full` on the machine the deploy just built: the image line names the version and the tag, which is
  # the label comparison read back through its other caller — and it runs the self-test, which is the measurement
  # that the agent image can be a session's image at all. This one is deliberately **not** given the shim: a
  # Tailscale that cannot be asked is `unknown` here, which is not a failure, and the deploy above is where the
  # origins were measured.
  status=0
  "$artifact" doctor --full --config "$root/install/config.toml" --root "$root/install" \
    >"$WORK/$name.doctor.out" 2>&1 || status=$?
  [ "$status" -eq 0 ] ||
    fail "$name/doctor/exit" "doctor --full exited $status: $(tail -20 "$WORK/$name.doctor.out")"
  assertions=$((assertions + 1))
  line="$(grep '^image: pass: ' "$WORK/$name.doctor.out" || true)"
  [ -n "$line" ] || fail "$name/doctor/image" "no image pass: $(cat "$WORK/$name.doctor.out")"
  case "$line" in
    *"$version"*"$tag"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/doctor/labels" "the image pass names neither the version nor the git tag: $line" ;;
  esac

  # Its own containers and its scratch go, and so do the two the deploy's step 4 started: `hs_key` mounts a
  # directory inside this case's tree, so removing the tree under a running container would leave it crash-looping
  # on a mount source that is no longer there. **Both images stay** (see this case's comment), which is what a later
  # `serve` boot needs to start the proxies again. The responder goes first: it is this case's listener, and the next
  # case picks its own port.
  kill "$holder_pid" 2>/dev/null || true
  wait "$holder_pid" 2>/dev/null || true
  holder_pid=""
  timeout 60 docker rm -f "hs_images_egress_$$" "hs_images_key_$$" hs_egress hs_key >/dev/null 2>&1 || true
  rm -rf "$root"
}

# The release build runs in the pinned builder container, and the host only orchestrates Docker.
#
# The first arm is the one that can fail on the thing the story is about: a `docker` stand-in on `PATH` records every
# argv and writes a **real** static artifact into the `--out` mount, while `cargo` and `rustc` stand-ins are also on
# `PATH` and record any call — so "the host compiled nothing" is an assertion, not a hope. It measures the recipe the
# image is built from (the `--build-arg` carries the digest of the two embedded files), the two mounts, the tag the
# container is handed, the placement of the artifact and its `.sha256`, an idempotent re-run, a refusal when the
# directory already holds a different binary, and a refusal when the artifact is for another machine.
#
# The second arm runs the real container when this machine has a daemon **and** the builder image is already built:
# it asserts what only a real build can — `file` says statically linked, `readelf -d` finds no dynamic section, and
# the digest is the file's. Without them it prints one note and the first arm's measurements stand, because a `SKIP`
# here would fire exactly when the feature is deleted (the gate rule) and the first arm already fails on that.
# the smoke run — a candidate binary proving itself against the real daemon through a fake pi.
#
# Three arms and **no SKIP**. The first two are daemon-free and always run, so deleting the verb, the subcommand, the
# fixture or the smoke config fails this case on any machine: the artifact's own `container fake-pi` over two pipes
# (the recorded turn, its echoed ids and its `agent_settled`), and a `docker` stand-in that answers about itself and
# refuses the image build, which must be exit **1** with the harness's own sentence and a smoke config on disk with
# the four settings the acceptance names. The third arm is the real run — the candidate served, a session created, a
# turn whose frames are asserted, the delete and the names gone — and prints a `note` (never a SKIP) when this machine
# has no daemon, because a note withholds the `ok` line that says something was measured.
case_smoke() {
  local name=case_smoke root="$repo_root/target/verify-smoke-$$" bin fake
  local answer code pin usage containers networks proxies_before proxies_after first
  # A second statement, because `local a=$x b=$a/b` expands every word *before* it assigns anything: under `set -u`
  # that is `b: unbound variable`, which is how this case first failed.
  bin="$root/bin"
  rm -rf "$root"
  mkdir -p "$bin"

  # Arm 1: the artifact's own fake pi, over two pipes. `--version` is the pin, because the image's `pi` is what
  # `agent::version_of` asks; the two commands are a handshake and a prompt, and what must come out is the recorded
  # turn — never the header line, never the recording's own command id (`req-1`), and always `agent_settled` last.
  pin="$(tr -d '[:space:]' <"$repo_root/pins/pi.txt")"
  answer="$("$artifact" container fake-pi --version)" || fail "$name/version" "the fake refused --version"
  check "$name/version" "$pin" "$(printf '%s' "$answer" | tr -d '[:space:]')"

  fake="$(printf '%s\n' \
    '{"id":"ready-1","type":"get_state"}' \
    '{"id":"turn-1","type":"prompt","message":"replay the recorded turn"}' |
    "$artifact" container fake-pi)" || fail "$name/fake_pi" "the fake pi did not answer"
  # One line, two fields, in whatever order the encoder wrote them: serde_json sorts an object's keys, so a pattern
  # that pinned `id` before `command` asserted the encoder rather than the answer (measured: this case's first run).
  first="$(printf '%s\n' "$fake" | sed -n '1p')"
  case "$first" in
    *'"id":"ready-1"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/handshake" "the first answer does not carry the id that was sent: $first" ;;
  esac
  case "$first" in
    *'"command":"get_state"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/handshake" "the first answer is not the get_state response: $first" ;;
  esac
  case "$fake" in
    *'"id":"turn-1"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/prompt" "no response echoed the prompt's own id" ;;
  esac
  case "$fake" in
    *'"command":"prompt"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/prompt" "no answer named the prompt command" ;;
  esac
  case "$fake" in
    *'"type":"fixture"'*) fail "$name/header" "the fixture's header line reached the client" ;;
    *'"id":"req-1"'*) fail "$name/recorded_id" "the recording's own command id was replayed as an answer" ;;
    *'"type":"tool_execution_start"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/tool" "the replayed turn holds no tool call" ;;
  esac
  check "$name/settled" '{"type":"agent_settled","aborted":false}' "$(printf '%s\n' "$fake" | tail -1)"
  usage="$(printf '%s\n' "$fake" | grep -o '"totalTokens":[0-9]*' | sed 's/.*://' | sort -n | tail -1)"
  [ -n "$usage" ] && [ "$usage" -gt 0 ] ||
    fail "$name/usage" "the replayed turn reports no usage: '$usage'"

  # Arm 2: a daemon that answers about itself and refuses to build the smoke image. Exit **1**, the harness's own
  # sentence, and the smoke config on disk carrying the four settings that keep the run off the machine's state.
  cat >"$bin/docker" <<'SHIM'
#!/bin/sh
case "$1" in
  version) printf 'amd64\n' ;;
  image) exit 1 ;;
  build) cat >/dev/null; printf 'the stand-in daemon refused the smoke image\n' >&2; exit 1 ;;
  *) exit 1 ;;
esac
SHIM
  chmod 0755 "$bin/docker"
  code=0
  answer="$(PATH="$bin:$PATH" "$artifact" --config "$WORK/config.toml" smoke --binary "$artifact" \
    --root "$root/config" 2>&1)" || code=$?
  check "$name/build_exit" 1 "$code"
  case "$answer" in
    *"the stand-in daemon refused the smoke image"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/build_sentence" "the refusal did not carry the daemon's own words: $answer" ;;
  esac
  [ -f "$root/config/config.toml" ] ||
    fail "$name/config" "no smoke config was written before the build: $answer"
  grep -qx 'container_prefix = "smoke_"' "$root/config/config.toml" ||
    fail "$name/container_prefix" "the smoke config does not prefix containers: $(cat "$root/config/config.toml")"
  grep -qx 'network_prefix = "smoke_"' "$root/config/config.toml" ||
    fail "$name/network_prefix" "the smoke config does not prefix networks: $(cat "$root/config/config.toml")"
  grep -qx 'control_listen = "127.0.0.1:0"' "$root/config/config.toml" ||
    fail "$name/port" "the smoke config does not ask for a port of its own: $(cat "$root/config/config.toml")"
  grep -q "^data_root = \"$root/config/data\"$" "$root/config/config.toml" ||
    fail "$name/data_root" "the smoke config's data root is not the run's own: $(cat "$root/config/config.toml")"
  assertions=$((assertions + 4))

  # Arm 3: the real thing.
  if ! command -v docker >/dev/null 2>&1 || ! timeout 60 docker version >/dev/null 2>&1; then
    printf 'note %s: no Docker daemon answers, so the real smoke run was not measured (arms 1 and 2 were)\n' "$name"
    rm -rf "$root"
    return 0
  fi
  # The machine's own two proxies, by **id**: a start `rm -f`s and re-runs both, and a replaced container keeps its
  # name — only the id says whether it is the same one. They are made here rather than assumed, because
  # `case_server_images` removed them and an empty string would compare equal to itself: a vacuous check that this
  # ADR cites is the pattern this repository keeps re-finding (S2).
  sentinel="$(docker images --format '{{.Repository}}:{{.Tag}}' | grep -E '^(holosmith-proxy:|alpine:)' | head -1)"
  proxies_before=""
  if [ -n "$sentinel" ] && docker create --name hs_egress "$sentinel" >/dev/null 2>&1 &&
    docker create --name hs_key "$sentinel" >/dev/null 2>&1; then
    proxies_before="$(docker inspect --format '{{.Id}}' hs_egress hs_key | tr '\n' ' ')"
  else
    printf 'note %s: no local image to make the two proxy sentinels from, so "the machine proxies were not replaced" was not measured\n' "$name"
  fi
  code=0
  answer="$("$artifact" --config "$WORK/config.toml" smoke --binary "$artifact" --root "$root/real" 2>&1)" ||
    code=$?
  check "$name/exit" 0 "$code"
  for line in 'smoke: image: holosmith-smoke:' 'smoke: session: ' 'smoke: turn: ' 'smoke: delete: 202 then 404' \
    'smoke: gone: smoke_s_' 'smoke: proxies: smoke_hs_egress, smoke_hs_key removed' 'smoke: ok'; do
    case "$answer" in
      *"$line"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/report" "the run did not print '$line': $answer" ;;
    esac
  done
  case "$answer" in
    *'one tool call, usage '*) assertions=$((assertions + 1)) ;;
    *) fail "$name/turn" "the run did not report the turn's tool call and usage: $answer" ;;
  esac
  # Nothing of the run's own is left, and the machine's proxies are the containers they were.
  containers="$(docker ps -aq --filter 'name=smoke_' 2>/dev/null || true)"
  [ -z "$containers" ] || fail "$name/leftover_container" "a smoke container outlived the run: $containers"
  networks="$(docker network ls -q --filter 'name=smoke_' 2>/dev/null || true)"
  [ -z "$networks" ] || fail "$name/leftover_network" "a smoke network outlived the run: $networks"
  assertions=$((assertions + 2))
  if [ -n "$proxies_before" ]; then
    proxies_after="$(docker inspect --format '{{.Id}}' hs_egress hs_key 2>/dev/null | tr '\n' ' ' || true)"
    [ -n "$proxies_after" ] ||
      fail "$name/machine_proxies" "the machine's two sentinel proxies are gone: the smoke run took them"
    check "$name/machine_proxies" "$proxies_before" "$proxies_after"
  fi
  docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  # The root is the run's scratch space and goes with it, so a second run cannot inherit a first one's database.
  [ ! -e "$root/real" ] || fail "$name/root" "the run left its root behind: $root/real"

  rm -rf "$root"
}

# `holosmith upgrade <binary>`, the promotion pipeline (spec item 10, ADR-0082).
#
# Four arms. Two need nothing but the artifact — the review gate is a refusal, and a candidate compared against a
# database one schema newer is a refusal, both **before** anything is asked of the machine. Two need the daemon and a
# serving machine: the real pipeline (a `systemctl` stand-in that starts the release `current` names, a `docker`
# stand-in that fakes **only** the two product images so the real smoke run still measures the candidate, and a real
# drain over a real socket), and the rollback — the same promotion with a stand-in whose first restart starts nothing
# that can serve, so the 20-second poll fails and the symlink goes back by itself.
#
# What the order is measured by: the printed step lines (each is printed by the step that does the work), the
# **recorded children** (the two `docker build`s, then the `systemctl restart`), and the effects — a session the drain
# stopped is `idle` again after the restart, and a backup exists before `current` moves. What it cannot measure is a
# real systemd: there is none on this machine, so the "restart" is the stand-in's own start of the binary `current`
# names, and this comment is where that is disclosed.
case_upgrade() {
  local name=case_upgrade root="$repo_root/target/verify-upgrade-$$"
  local data="$root/data" projects="$root/projects" admin="$root/data/run" config="$root/config.toml"
  local bin="$root/bin" candidate="$root/candidate/holosmith" state="$root/state"
  local docker_log="$root/docker.argv" systemctl_log="$root/systemctl.argv" serve_log="$root/serve.log"
  local pid_file="$root/serve.pid" broken="$root/broken" version tag digest status answer port line schema
  local first second session snapshots
  rm -rf "$root"
  mkdir -p "$bin" "$data" "$projects" "$admin" "$root/candidate" "$state"

  # The candidate is this tree's artifact with the two files `holosmith build` writes beside each other, so the
  # promotion's own ELF check, digest check and `version --json` are all measured over real bytes.
  cp "$artifact" "$candidate"
  digest="$(sha256sum "$candidate" | cut -d' ' -f1)"
  printf '%s  holosmith\n' "$digest" >"$root/candidate/holosmith.sha256"
  version="$("$artifact" version | sed -n 's/^version //p')"
  tag="$("$artifact" version | sed -n 's/^git_tag //p')"
  [ -n "$version" ] && [ -n "$tag" ] || fail "$name/version" "the artifact reports no version or tag"
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "stub"\nrole = "server"\nprojects_root = "%s"\ndata_root = "%s"\nmin_free_gb = 0\nsession_disk_soft_gb = 0\nidle_stop_min = 0\npreview_listen = "127.0.0.1:0"\n' \
    "$owner" "$projects" "$data" >"$config"

  # ---- 1. the review gate is a refusal, and it happens before anything is asked ---------------------
  status=0
  answer="$("$artifact" --config "$config" upgrade "$candidate" --root "$root" 2>&1)" || status=$?
  check "$name/review/exit" 1 "$status"
  case "$answer" in
    *"review gate"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/review/sentence" "the refusal does not name the gate: $answer" ;;
  esac
  [ ! -e "$root/opt" ] ||
    fail "$name/review/release" "a refused promotion wrote a release tree: $(find "$root" -mindepth 1 -maxdepth 2 | head -3)"
  [ ! -e "$data/backups" ] ||
    fail "$name/review/backup" "a refused promotion wrote a backup: $(find "$data/backups" -mindepth 1 | head -3)"

  # ---- 2. a candidate older than the database is refused ------------------------------------------
  # One more than the schema the **artifact** reports, read from its own `version --json`: a hard-coded number stops
  # being "newer" the day this build ships that schema, and this arm would then fail for the wrong reason.
  schema="$("$artifact" version | sed -n 's/^schema_version //p')"
  if [ -n "$schema" ] && command -v python3 >/dev/null 2>&1; then
    python3 - "$data/app.db" "$schema" <<'PY'
import sqlite3, sys
path, newer = sys.argv[1], int(sys.argv[2]) + 1
connection = sqlite3.connect(path)
connection.execute(
    "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL)"
)
for version in range(1, newer + 1):
    connection.execute(
        "INSERT OR REPLACE INTO schema_version (version, name, applied_at) VALUES (?, 'a release', 0)",
        (version,),
    )
connection.commit()
PY
    status=0
    answer="$("$artifact" --config "$config" upgrade "$candidate" --acknowledge-review --root "$root" 2>&1)" ||
      status=$?
    check "$name/schema/exit" 1 "$status"
    case "$answer" in
      *"older than the database"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/schema/sentence" "the refusal does not name the schema: $answer" ;;
    esac
    [ ! -e "$root/opt" ] ||
      fail "$name/schema/release" "a refused candidate wrote a release tree"
    [ ! -e "$data/backups" ] ||
      fail "$name/schema/backup" "a refused candidate wrote a backup"
    rm -f "$data/app.db"
  else
    printf 'note %s: no python3 to make a database from a newer release, so the schema refusal was not measured\n' "$name"
  fi

  # Everything below starts a server whose port the promotion's health poll reads from the config, so the two have to
  # agree: `--port 0` is what re-derives a control origin, and a config's port does not (ADR-0007), so the origin is
  # spelled here as the listener is.
  if ! command -v docker >/dev/null 2>&1 || ! timeout 60 docker version >/dev/null 2>&1; then
    printf 'note %s: no Docker daemon answers, so the promotion and its rollback were not measured (the two refusals were)\n' "$name"
    rm -rf "$root"
    return 0
  fi
  port="$(free_port)"
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "stub"\nrole = "server"\ncontrol_listen = "127.0.0.1:%s"\ncontrol_origin = "http://127.0.0.1:%s"\nprojects_root = "%s"\nmin_free_gb = 0\nsession_disk_soft_gb = 0\nidle_stop_min = 0\npreview_listen = "127.0.0.1:0"\nkeep_releases = 3\ndata_root = "%s"\n' \
    "$owner" "$port" "$port" "$projects" "$data" >"$config"

  # The stand-ins. `docker` fakes the **two product images** and nothing else: every other question — the smoke run's
  # own image, its session's container and network — goes to the real daemon, so the real smoke test still runs. The
  # labels it remembers are the `--build-arg` pairs it was given, which is how the *second* promotion reads the images
  # as current: acceptance bullet 4's "only when the binary or a pinned version changed", in both directions.
  cat >"$bin/docker" <<SHIM
#!/bin/sh
{
  printf 'docker'
  for argument in "\$@"; do printf ' %s' "\$argument"; done
  printf '\n'
} >>"$docker_log"
case "\$1" in
  build)
    tag=""
    previous=""
    json=""
    for argument in "\$@"; do
      [ "\$previous" = "-t" ] && tag="\$argument"
      previous="\$argument"
      case "\$argument" in
        HOLOSMITH_VERSION=*) json="\$json\"org.holosmith.version\":\"\${argument#*=}\"," ;;
        HOLOSMITH_GIT_TAG=*) json="\$json\"org.holosmith.git-tag\":\"\${argument#*=}\"," ;;
        HOLOSMITH_PI_VERSION=*) json="\$json\"org.holosmith.pi-version\":\"\${argument#*=}\"," ;;
        HOLOSMITH_BINARY_SHA256=*) json="\$json\"org.holosmith.binary-sha256\":\"\${argument#*=}\"," ;;
        HOLOSMITH_TOOLCHAINS=*) json="\$json\"org.holosmith.toolchains\":\"\${argument#*=}\"," ;;
      esac
    done
    case "\$tag" in
      holosmith-proxy:*|agent-image:*)
        cat >/dev/null
        printf '{%s"faked":"yes"}' "\$json" >"$state/\$tag.json"
        printf 'faked the build of %s\n' "\$tag"
        exit 0
        ;;
    esac
    ;;
  image)
    tag=""
    format=""
    for argument in "\$@"; do
      case "\$argument" in
        holosmith-proxy:*|agent-image:*) tag="\$argument" ;;
        *Config.Labels*) format="\$argument" ;;
      esac
    done
    case "\$tag" in
      holosmith-proxy:*|agent-image:*)
        [ -f "$state/\$tag.json" ] || exit 1
        case "\$format" in
          "") printf 'sha256:faked\n' ;;
          *) cat "$state/\$tag.json" ;;
        esac
        exit 0
        ;;
    esac
    ;;
esac
exec "$(command -v docker)" "\$@"
SHIM
  # `systemctl` is what systemd would be: on `restart` it stops what is running and starts the binary the unit's
  # `ExecStart` names — read through `current`, which is the whole point of the layout. \`HS_BREAK_FIRST\` is the
  # failure the rollback arm injects: the first restart starts a `serve` with no config, so nothing answers the
  # candidate's health poll and the promotion has to put the previous release back by itself.
  cat >"$bin/systemctl" <<SHIM
#!/bin/sh
{
  printf 'systemctl'
  for argument in "\$@"; do printf ' %s' "\$argument"; done
  printf '\n'
} >>"$systemctl_log"
case "\$1" in
  daemon-reload) exit 0 ;;
  restart)
    if [ -f "$pid_file" ]; then
      old="\$(cat "$pid_file")"
      kill "\$old" 2>/dev/null || true
      for _ in \$(seq 1 100); do kill -0 "\$old" 2>/dev/null || break; sleep 0.05; done
    fi
    exe="\$(readlink -f "$root/current")/holosmith"
    if [ "\${HS_BREAK_FIRST:-0}" = 1 ] && [ ! -f "$broken" ]; then
      : >"$broken"
      "\$exe" serve --config /nonexistent/holosmith.toml >>"$serve_log" 2>&1 &
    else
      "\$exe" serve --config "$config" --admin-dir "$admin" >>"$serve_log" 2>&1 &
    fi
    printf '%s\n' "\$!" >"$pid_file"
    exit 0
    ;;
esac
exit 0
SHIM
  # `sudo`: the release verbs restart through `sudo -n systemctl restart <unit>` (ADR-0086 D4), and a real `sudo`
  # resets PATH — so the `systemctl` above would never be reached, and on a machine without
  # `/etc/sudoers.d/holosmith` it answers "a password is required" and the promotion has nothing to poll. A gate
  # cannot install that line, so this stand-in is what a machine with it does: drop `-n` and run the program this
  # case's PATH names.
  cat >"$bin/sudo" <<'SUDO'
#!/bin/sh
[ "$1" = "-n" ] && shift
exec "$@"
SUDO
  chmod 0755 "$bin/docker" "$bin/systemctl" "$bin/sudo"

  # A machine that has promoted before: one release, `current` pointing at it, and the unit a deploy wrote through it.
  # That release is the promotion's **previous** one and what a rollback goes back to, so this case starts from the
  # state a second promotion is in rather than from a machine with no release tree at all.
  mkdir -p "$root/releases/old"
  cp "$candidate" "$root/releases/old/holosmith"
  cp "$root/candidate/holosmith.sha256" "$root/releases/old/holosmith.sha256"
  ln -s releases/old "$root/current"
  # the prune arm: two older releases (explicit mtimes, so the tree's order is this case's) and a directory the
  # tree does not recognise. After the promotion below the tree holds four releases and the config keeps three, so
  # exactly `ancient1` goes — the live release, the rollback target (`releases/old`) and the newest of the rest stay.
  for line in ancient1 ancient2; do
    mkdir -p "$root/releases/$line"
    cp "$candidate" "$root/releases/$line/holosmith"
    cp "$root/candidate/holosmith.sha256" "$root/releases/$line/holosmith.sha256"
  done
  touch -d @1700000001 "$root/releases/ancient1"
  touch -d @1700000002 "$root/releases/ancient2"
  mkdir -p "$root/releases/keepme"
  printf 'not a release: no holosmith here\n' >"$root/releases/keepme/README"
  printf '[Unit]\nDescription=Holosmith server\n\n[Service]\nExecStart=%s/current/holosmith serve\nRestart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n' \
    "$root" >"$root/holosmith.service"

  # The machine, serving: the same argv the stand-in replays, so the promotion's drain socket is this run's own.
  "$artifact" serve --config "$config" --admin-dir "$admin" >>"$serve_log" 2>&1 &
  extra_pid=$!
  printf '%s\n' "$extra_pid" >"$pid_file"
  for _ in $(seq 1 200); do
    (exec 3<>/dev/tcp/127.0.0.1/"$port") 2>/dev/null && break
    kill -0 "$extra_pid" 2>/dev/null || break
    sleep 0.05
  done
  # One idle session, which the drain stops and the boot after each restart has to bring back.
  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1:$port" "Origin: http://127.0.0.1:$port" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-upgrade-1" ||
    fail "$name/create" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  session="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/${name}_create.body")"
  [ -n "$session" ] || fail "$name/create/id" "the response names no id: $(cat "$WORK/responses/${name}_create.body")"
  for _ in $(seq 1 400); do
    fetch "${name}_idle" GET "/api/sessions/$session" "Host: 127.0.0.1:$port" || true
    case "$(answer "${name}_idle")" in
      *'"status":"idle"'*) break ;;
    esac
    sleep 0.05
  done
  case "$(answer "${name}_idle")" in
    *'"status":"idle"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/idle" "the session never reached idle: $(cat "$WORK/responses/${name}_idle.body")" ;;
  esac

  # ---- 3. the rollback: a candidate whose new version never answers ---------------------------------
  status=0
  HS_BREAK_FIRST=1 PATH="$bin:$PATH" "$artifact" --config "$config" upgrade "$candidate" --acknowledge-review \
    --root "$root" --wait 30s >"$WORK/$name.rollback.out" 2>&1 || status=$?
  check "$name/rollback/exit" 1 "$status"
  if [ -f "$pid_file" ]; then extra_pid="$(cat "$pid_file")"; fi
  case "$(cat "$WORK/$name.rollback.out")" in
    *"rolled back"*"is live again"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/rollback/sentence" "the rollback did not say what is live now: $(tail -5 "$WORK/$name.rollback.out")" ;;
  esac
  grep -q '^upgrade: health: no answer with ' "$WORK/$name.rollback.out" ||
    fail "$name/rollback/health" "the poll did not report giving up: $(tail -5 "$WORK/$name.rollback.out")"
  [ "$(grep -c '^systemctl restart holosmith.service$' "$systemctl_log")" -eq 2 ] ||
    fail "$name/rollback/restarts" "the rollback did not restart the service a second time: $(cat "$systemctl_log")"
  check "$name/rollback/current" "releases/old" "$(readlink "$root/current")"
  grep -q "^upgrade: live: releases/old ($version, $tag) (the candidate was not made live), backup $data/backups/app-" \
    "$WORK/$name.rollback.out" ||
    fail "$name/rollback/live" "the rollback's last line does not name the release and version that are live: $(tail -3 "$WORK/$name.rollback.out")"
  # The session the drain stopped came back with the release that was switched back to.
  for _ in $(seq 1 400); do
    fetch "${name}_resumed" GET "/api/sessions/$session" "Host: 127.0.0.1:$port" || true
    case "$(answer "${name}_resumed")" in
      *'"status":"idle"'*) break ;;
    esac
    sleep 0.05
  done
  case "$(answer "${name}_resumed")" in
    *'"status":"idle"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/resumed" "the session the drain stopped did not come back idle: $(cat "$WORK/responses/${name}_resumed.body")" ;;
  esac
  assertions=$((assertions + 2))

  # ---- 4. the promotion, in the spec's order ------------------------------------------------------
  status=0
  PATH="$bin:$PATH" "$artifact" --config "$config" upgrade "$candidate" --acknowledge-review \
    --root "$root" --wait 30s >"$WORK/$name.run.out" 2>&1 || status=$?
  check "$name/run/exit" 0 "$status"
  if [ -f "$pid_file" ]; then extra_pid="$(cat "$pid_file")"; fi

  # The steps, in the spec's order. Each line is printed by the step that does the work, so the order of the lines is
  # the order of the work; the recorded children below back the three that have one.
  first=0
  for line in "smoke: passed" "images: " "drain: drained" "backup: " "release: " "current: " "restart: " "health: " "live: "; do
    second="$(sed -n "/^upgrade: $line/=" "$WORK/$name.run.out" | head -1)"
    [ -n "$second" ] || fail "$name/order" "no 'upgrade: $line' line: $(cat "$WORK/$name.run.out")"
    [ "$second" -gt "$first" ] || fail "$name/order" "'upgrade: $line' is out of order: $(cat "$WORK/$name.run.out")"
    first="$second"
    assertions=$((assertions + 1))
  done
  # The images were built by the promotion before this one, so this one reads them as current: the acceptance's "only
  # when the binary or a pinned version changed", measured by a second promotion of the same candidate.
  grep -q '^upgrade: images: no image needed building$' "$WORK/$name.run.out" ||
    fail "$name/images/current" "a second promotion rebuilt the images: $(cat "$WORK/$name.run.out")"
  [ "$(grep -c -- "^docker build -t holosmith-proxy:$version" "$docker_log")" -eq 1 ] ||
    fail "$name/images/once" "the proxy image was built more than once for one candidate: $(cat "$docker_log")"
  grep -qF -- "HOLOSMITH_BINARY_SHA256=$digest" "$docker_log" ||
    fail "$name/images/digest" "the images were not built from the candidate's bytes: $(cat "$docker_log")"
  grep -q '^systemctl restart holosmith.service$' "$systemctl_log" ||
    fail "$name/restart" "the promotion did not restart the unit: $(cat "$systemctl_log")"
  # The restart is the **last** child: after the drain and after the images.
  tail -1 "$systemctl_log" | grep -qx 'systemctl restart holosmith.service' ||
    fail "$name/restart/last" "the restart is not the last child the promotion ran: $(cat "$systemctl_log")"
  assertions=$((assertions + 3))

  # The effects: the release, the link, the backups, and the session the drain stopped.
  check "$name/current" "releases/$tag" "$(readlink "$root/current")"
  cmp -s "$root/releases/$tag/holosmith" "$candidate" ||
    fail "$name/release" "the release's binary is not the candidate's bytes"
  snapshots="$(ls "$data"/backups/app-*.db 2>/dev/null | wc -l)"
  [ "$snapshots" -eq 2 ] ||
    fail "$name/backup" "two promotions did not write two snapshots: $(ls "$data/backups" 2>/dev/null)"
  # The newest snapshot is a database, not a note: SQLite's own fifteen-byte header, read from the file.
  for snapshot in "$data"/backups/app-*.db; do
    head -c 15 "$snapshot" | grep -q 'SQLite format 3' ||
      fail "$name/backup/db" "a snapshot is not a database: $snapshot"
    assertions=$((assertions + 1))
  done
  case "$(cat "$WORK/$name.run.out")" in
    *"stopped: 1 session with reason server_restart"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/drain/session" "the drain did not stop the idle session: $(cat "$WORK/$name.run.out")" ;;
  esac
  for _ in $(seq 1 400); do
    fetch "${name}_back" GET "/api/sessions/$session" "Host: 127.0.0.1:$port" || true
    case "$(answer "${name}_back")" in
      *'"status":"idle"'*) break ;;
    esac
    sleep 0.05
  done
  case "$(answer "${name}_back")" in
    *'"status":"idle"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/back" "the session the drain stopped did not come back idle: $(cat "$WORK/responses/${name}_back.body")" ;;
  esac
  # The last line: the version that is now live, and the backup this promotion took.
  grep -q "^upgrade: live: $version ($tag), backup $data/backups/app-" "$WORK/$name.run.out" ||
    fail "$name/live" "the last line does not name the version and the backup: $(tail -3 "$WORK/$name.run.out")"
  assertions=$((assertions + 2))

  # the prune is the promotion's last step, and it is the tree's own order that decides. Four releases, three
  # kept: the live one and the rollback target (`releases/old`, which the print above names) are reserved, the newest
  # of the rest stays, and `ancient1` is the one that goes.
  grep -q '^upgrade: prune: kept 3 of 4 (keep_releases = 3); removed: releases/ancient1$' "$WORK/$name.run.out" ||
    fail "$name/prune/line" "the promotion did not prune: $(tail -3 "$WORK/$name.run.out")"
  assertions=$((assertions + 1))
  # It is the *last* line, which is what "the promotion's last step" means: a prune before the health poll would print
  # above `live:` and leave a failed promotion with a pruned tree.
  check "$name/prune/last" "upgrade: prune: kept 3 of 4 (keep_releases = 3); removed: releases/ancient1" \
    "$(tail -1 "$WORK/$name.run.out")"
  [ ! -e "$root/releases/ancient1" ] ||
    fail "$name/prune/removed" "the oldest release survived the prune: $(ls "$root/releases")"
  assertions=$((assertions + 1))
  for line in "$tag" old ancient2; do
    [ -d "$root/releases/$line" ] ||
      fail "$name/prune/kept" "the prune removed releases/$line: $(ls "$root/releases")"
    assertions=$((assertions + 1))
  done
  # A directory the tree does not recognise is not a release: it is not counted and never deleted.
  [ -f "$root/releases/keepme/README" ] ||
    fail "$name/prune/unrecognised" "the prune removed a directory that is not a release"
  assertions=$((assertions + 1))

  kill "$extra_pid" 2>/dev/null || true
  extra_pid=""
  # Nothing of the run's own is left on the machine: the smoke run removed its container, its network and its proxies.
  [ -z "$(docker ps -aq --filter 'name=smoke_' 2>/dev/null || true)" ] ||
    fail "$name/smoke/container" "a smoke container outlived the promotion"
  [ -z "$(docker network ls -q --filter 'name=smoke_' 2>/dev/null || true)" ] ||
    fail "$name/smoke/network" "a smoke network outlived the promotion"
  assertions=$((assertions + 2))

  rm -rf "$root"
}

# the rollback verb. Four arms, and the interesting ones need neither a daemon nor an image: a rollback runs no
# smoke test, builds nothing and takes no backup, so what is measured here is the *release tree*, the drain, one
# restart and one health answer. The `systemctl` stand-in does what systemd would (stop what runs, start what
# `current` names) and records what `current` named at the moment it restarted, which is how "drains first, points
# `current` at the previous release, restarts" becomes an order the artifact's own output and the tree prove.
case_rollback() {
  local name=case_rollback root="$repo_root/target/verify-rollback-$$"
  local data="$root/data" projects="$root/projects" admin="$root/data/run" config="$root/config.toml"
  local bin="$root/bin" systemctl_log="$root/systemctl.argv" serve_log="$root/serve.log"
  local pid_file="$root/serve.pid" restart_view="$root/restart.current"
  local releases="$root/releases" link="$root/current"
  local unit="$root/holosmith.service"
  local version tag digest status answer port line first second session schema
  rm -rf "$root"
  mkdir -p "$bin" "$data" "$projects" "$admin"

  # The release every arm moves between: this tree's artifact, because a rollback reads its target's own
  # `version --json` — the version the poll waits for and the schema the database is compared with.
  digest="$(sha256sum "$artifact" | cut -d' ' -f1)"
  version="$("$artifact" version | sed -n 's/^version //p')"
  tag="$("$artifact" version | sed -n 's/^git_tag //p')"
  [ -n "$version" ] && [ -n "$tag" ] || fail "$name/version" "the artifact reports no version or tag"
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "stub"\nrole = "server"\nprojects_root = "%s"\ndata_root = "%s"\nmin_free_gb = 0\nsession_disk_soft_gb = 0\nidle_stop_min = 0\npreview_listen = "127.0.0.1:0"\n' \
    "$owner" "$projects" "$data" >"$config"

  # `systemctl` is what systemd would be for this unit: it records its argv, records what `current` names, stops what
  # runs and starts the binary the link names. `HS_BREAK_RESTART=1` is the last arm's injection — the service is
  # stopped and nothing is started, so the rollback's own poll has nothing to find.
  cat >"$bin/systemctl" <<SHIM
#!/bin/sh
{
  printf 'systemctl'
  for argument in "\$@"; do printf ' %s' "\$argument"; done
  printf '\n'
} >>"$systemctl_log"
case "\$1" in
  daemon-reload) exit 0 ;;
  restart)
    readlink -f "$link" >"$restart_view"
    if [ -f "$pid_file" ]; then
      old="\$(cat "$pid_file")"
      kill "\$old" 2>/dev/null || true
      for _ in \$(seq 1 100); do kill -0 "\$old" 2>/dev/null || break; sleep 0.05; done
    fi
    [ "\${HS_BREAK_RESTART:-0}" = 1 ] && exit 0
    exe="\$(readlink -f "$link")/holosmith"
    "\$exe" serve --config "$config" --admin-dir "$admin" >>"$serve_log" 2>&1 &
    printf '%s\\n' "\$!" >"$pid_file"
    exit 0
    ;;
esac
exit 0
SHIM
  # `sudo`: the release verbs restart through `sudo -n systemctl restart <unit>` (ADR-0086 D4), and a real `sudo`
  # resets PATH, so this case's `systemctl` above would never be reached. See `case_upgrade`'s copy of this shim.
  cat >"$bin/sudo" <<'SUDO'
#!/bin/sh
[ "$1" = "-n" ] && shift
exec "$@"
SUDO
  chmod 0755 "$bin/systemctl" "$bin/sudo"

  # ---- 1. the trees a rollback cannot go back in, and a `--to` that names nothing --------------------------------
  # A machine that has never promoted: no `current` at all, so there is no release to go one back from.
  status=0
  answer="$(PATH="$bin:$PATH" "$artifact" --config "$config" rollback --root "$root/empty" 2>&1)" || status=$?
  check "$name/none/exit" 1 "$status"
  case "$answer" in
    *"has never promoted"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/none/sentence" "the refusal does not say the machine never promoted: $answer" ;;
  esac
  [ ! -e "$root/empty" ] || fail "$name/none/tree" "a refused rollback wrote an install tree"

  # One release, `current` on it: there is nothing behind it, and the sentence names the flag that could ask for one.
  mkdir -p "$root/single/releases/only"
  cp "$artifact" "$root/single/releases/only/holosmith"
  ln -s releases/only "$root/single/current"
  status=0
  answer="$(PATH="$bin:$PATH" "$artifact" --config "$config" rollback --root "$root/single" 2>&1)" || status=$?
  check "$name/only/exit" 1 "$status"
  case "$answer" in
    *"oldest release this tree holds"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/only/sentence" "the one-release refusal does not say it is the oldest: $answer" ;;
  esac
  check "$name/only/link" "releases/only" "$(readlink "$root/single/current")"

  # The tree the served arms use: three releases, written in the order `one`, `two`, `three`, with `current` on the
  # newest. The mtimes are set explicitly, because they are the tree's order — a tag is `git describe`'s answer and
  # does not sort (`f2b3886-dirty` sorts above `v0.1.0`), so a name order would walk into a hash.
  
  for line in one two three; do
    mkdir -p "$releases/$line"
    cp "$artifact" "$releases/$line/holosmith"
    printf '%s  holosmith\n' "$digest" >"$releases/$line/holosmith.sha256"
  done
  touch -d @1700000001 "$releases/one"
  touch -d @1700000002 "$releases/two"
  touch -d @1700000003 "$releases/three"
  ln -s releases/three "$link"
  printf '[Unit]\nDescription=Holosmith server\n\n[Service]\nExecStart=%s/current/holosmith serve\nRestart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n' \
    "$root" >"$unit"
  [ ! -s "$systemctl_log" ] || fail "$name/early/restart" "a refused rollback restarted the service: $(cat "$systemctl_log")"

  # A tag that is not in the tree: the refusal lists the ones that are, so an operator can copy one out of it.
  status=0
  answer="$(PATH="$bin:$PATH" "$artifact" --config "$config" rollback --root "$root" --to nowhere 2>&1)" || status=$?
  check "$name/to/exit" 1 "$status"
  case "$answer" in
    *"names no release in this tree"*"three, two, one"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/to/sentence" "the --to refusal does not list the tree in its own order: $answer" ;;
  esac
  check "$name/to/link" "releases/three" "$(readlink "$link")"

  # ---- 2. a database from a newer release is refused before the drain, with the link where it was ----------------
  schema="$("$artifact" version | sed -n 's/^schema_version //p')"
  if [ -n "$schema" ] && command -v python3 >/dev/null 2>&1; then
    python3 - "$data/app.db" "$schema" <<'PY'
import sqlite3, sys
path, newer = sys.argv[1], int(sys.argv[2]) + 1
connection = sqlite3.connect(path)
connection.execute(
    "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL)"
)
for version in range(1, newer + 1):
    connection.execute(
        "INSERT OR REPLACE INTO schema_version (version, name, applied_at) VALUES (?, 'a release', 0)",
        (version,),
    )
connection.commit()
PY
    status=0
    answer="$(PATH="$bin:$PATH" "$artifact" --config "$config" rollback --root "$root" 2>&1)" || status=$?
    check "$name/schema/exit" 1 "$status"
    case "$answer" in
      *"older than the database"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/schema/sentence" "the refusal does not name the schema: $answer" ;;
    esac
    # The acceptance's "with a clear explanation": the pre-line that would refuse it after a restart, and the rule
    # that makes a rollback safe without a restore.
    case "$answer" in
      *"doctor --quick"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/schema/quick" "the refusal does not name the pre-line that would refuse it: $answer" ;;
    esac
    check "$name/schema/link" "releases/three" "$(readlink "$link")"
    [ ! -s "$systemctl_log" ] || fail "$name/schema/restart" "a refused rollback restarted the service: $(cat "$systemctl_log")"
    [ ! -s "$restart_view" ] || fail "$name/schema/view" "a refused rollback recorded a restart: $(cat "$restart_view")"
    # The database the served arms use is this build's own: the boot below writes it.
    rm -f "$data/app.db"
  else
    printf 'note %s: no python3 to make a database from a newer release, so the schema refusal was not measured\n' "$name"
  fi

  # Everything below needs a machine serving on the port the config names, so the origin is spelled to match it: a
  # config's port does not re-derive `control_origin` (`--port 0` does).
  port="$(free_port)"
  printf 'owner = "%s"\nmax_live_sessions = 2\nbackend = "stub"\nrole = "server"\ncontrol_listen = "127.0.0.1:%s"\ncontrol_origin = "http://127.0.0.1:%s"\nprojects_root = "%s"\nmin_free_gb = 0\nsession_disk_soft_gb = 0\nidle_stop_min = 0\npreview_listen = "127.0.0.1:0"\ndata_root = "%s"\n' \
    "$owner" "$port" "$port" "$projects" "$data" >"$config"

  # The machine, serving: the same argv the stand-in replays, so the rollback's drain socket is this run's own.
  "$artifact" serve --config "$config" --admin-dir "$admin" >>"$serve_log" 2>&1 &
  extra_pid=$!
  printf '%s\n' "$extra_pid" >"$pid_file"
  for _ in $(seq 1 200); do
    (exec 3<>/dev/tcp/127.0.0.1/"$port") 2>/dev/null && break
    kill -0 "$extra_pid" 2>/dev/null || break
    sleep 0.05
  done
  # One idle session, which the drain stops and the boot after the restart has to bring back.
  fetch_body "${name}_create" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1:$port" "Origin: http://127.0.0.1:$port" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-rollback-1" ||
    fail "$name/create" "POST /api/sessions did not answer"
  expect_status "${name}_create" "$(answer "${name}_create")" 202
  session="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/${name}_create.body")"
  [ -n "$session" ] || fail "$name/create/id" "the response names no id: $(cat "$WORK/responses/${name}_create.body")"
  for _ in $(seq 1 400); do
    fetch "${name}_idle" GET "/api/sessions/$session" "Host: 127.0.0.1:$port" || true
    case "$(answer "${name}_idle")" in
      *'"status":"idle"'*) break ;;
    esac
    sleep 0.05
  done
  case "$(answer "${name}_idle")" in
    *'"status":"idle"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/idle" "the session never reached idle: $(cat "$WORK/responses/${name}_idle.body")" ;;
  esac

  # ---- 3. the rollback: `current` moves back one release, and the machine serves it ------------------------------
  status=0
  PATH="$bin:$PATH" "$artifact" --config "$config" rollback --root "$root" --wait 30s \
    >"$WORK/$name.rollback.out" 2>&1 || status=$?
  check "$name/rollback/exit" 0 "$status"
  if [ -f "$pid_file" ]; then extra_pid="$(cat "$pid_file")"; fi

  # The steps, in the acceptance's order. Each line is printed by the step that does the work, so the order of the
  # lines is the order of the work — and the recorded restart below backs the one part of it a line cannot.
  first=0
  for line in "target: " "unit: " "drain: drained" "current: " "restart: " "health: " "live: "; do
    second="$(sed -n "/^rollback: $line/=" "$WORK/$name.rollback.out" | head -1)"
    [ -n "$second" ] || fail "$name/order" "no 'rollback: $line' line: $(cat "$WORK/$name.rollback.out")"
    [ "$second" -gt "$first" ] || fail "$name/order" "'rollback: $line' is out of order: $(cat "$WORK/$name.rollback.out")"
    first="$second"
    assertions=$((assertions + 1))
  done
  # The effects. One restart, and what `current` named when it ran: the switch is *before* the restart, which is what
  # makes the newly started process the release the rollback chose.
  check "$name/current" "releases/two" "$(readlink "$link")"
  [ "$(grep -c '^systemctl restart holosmith.service$' "$systemctl_log")" -eq 1 ] ||
    fail "$name/restarts" "the rollback did not restart the service exactly once: $(cat "$systemctl_log")"
  check "$name/switch/before" "$root/releases/two" "$(cat "$restart_view" 2>/dev/null)"
  # The drain stopped the session, and the boot the restart started brought it back — the acceptance's "sessions survive
  # the rollback in `idle` or `stopped`, never `error`".
  case "$(cat "$WORK/$name.rollback.out")" in
    *"stopped: 1 session with reason server_restart"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/drain/session" "the drain did not stop the idle session: $(cat "$WORK/$name.rollback.out")" ;;
  esac
  for _ in $(seq 1 400); do
    fetch "${name}_back" GET "/api/sessions/$session" "Host: 127.0.0.1:$port" || true
    case "$(answer "${name}_back")" in
      *'"status":"idle"'*) break ;;
    esac
    sleep 0.05
  done
  case "$(answer "${name}_back")" in
    *'"status":"idle"'*) assertions=$((assertions + 1)) ;;
    *) fail "$name/back" "the session the drain stopped did not come back idle: $(cat "$WORK/responses/${name}_back.body")" ;;
  esac
  grep -q "^rollback: live: releases/two ($version, $tag), serving$" "$WORK/$name.rollback.out" ||
    fail "$name/live" "the last line does not name the release that is serving: $(tail -3 "$WORK/$name.rollback.out")"
  assertions=$((assertions + 1))

  # ---- 4. a rollback whose own restart serves nothing says so loudly, and does not move the link again ----------
  status=0
  HS_BREAK_RESTART=1 PATH="$bin:$PATH" "$artifact" --config "$config" rollback --root "$root" --wait 30s \
    >"$WORK/$name.unanswered.out" 2>&1 || status=$?
  extra_pid=""
  check "$name/unanswered/exit" 1 "$status"
  case "$(cat "$WORK/$name.unanswered.out")" in
    *"systemctl status holosmith.service"*"journalctl -u holosmith.service"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/unanswered/next" "the failure names no next step: $(tail -4 "$WORK/$name.unanswered.out")" ;;
  esac
  # The way back forward is the same switch, and the sentence names the release this one came from (two, which is now
  # *newer* than the live one and is therefore legal for `--to`).
  case "$(cat "$WORK/$name.unanswered.out")" in
    *"--to two"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/unanswered/back" "the failure does not say how to go forward again: $(tail -4 "$WORK/$name.unanswered.out")" ;;
  esac
  check "$name/unanswered/current" "releases/one" "$(readlink "$link")"
  [ "$(grep -c '^systemctl restart holosmith.service$' "$systemctl_log")" -eq 2 ] ||
    fail "$name/unanswered/restarts" "the failed rollback did not restart the service once: $(cat "$systemctl_log")"

  kill "$extra_pid" 2>/dev/null || true
  extra_pid=""
  rm -rf "$root"
}

# what the release layout *means* once there is more than one release in it. No daemon, no database, no
# `python3` and no server: a release is "a copy of the artifact in a directory named by a tag", so four of them are
# four `cp`s and four mtimes, and everything this case measures is a file the artifact itself answers. The tree's own
# arm — the prune — is `case_upgrade`'s, because a prune only happens inside a promotion.
#
# `role = "hub"` is what keeps the five `doctor` runs free of the daemon: a hub is asked no Docker, git or merge
# question (item 10 §6), which is why this case needs no `_ping` shim and no `python3` — every other daemon-free case
# either uses `fake_docker_ping` or is a hub for the same reason, and a case whose `exit 0` arms went red on a machine
# with no daemon would be measuring the host instead of the tree.
#
# Three of the five assertions that matter are the acceptance's bullets 1, 2 and 3 read off the tree (`current` is a
# **relative** symlink, the unit's `ExecStart` goes through it, `version` names the directory the running file is in),
# and the fourth is bullet 5: `doctor --quick --root` in both arms of its release finding, plus the two arms where the
# answer is "this machine does not execute through the link" and "there is no unit to say what it runs".
case_releases() {
  local name=case_releases root="$repo_root/target/verify-releases-$$"
  local tree="$root" releases="$root/releases" link="$root/current"
  local unit="$root/holosmith.service" config="$root/config.toml"
  local digest status answer tag resolved line
  rm -rf "$root"
  install -d -m 0755 "$releases"

  # Four releases side by side, written by hand and given the mtimes that make the tree's order the test's. The
  # unrecognised directory is the **newest** thing in `releases/` on purpose: if `history()` counted it, the doctor's
  # remedy below would name it instead of `four`, so the filter is what that assertion measures.
  digest="$(sha256sum "$artifact" | cut -d' ' -f1)"
  for tag in one two three four; do
    install -d -m 0755 "$releases/$tag"
    cp "$artifact" "$releases/$tag/holosmith"
    printf '%s  holosmith\n' "$digest" >"$releases/$tag/holosmith.sha256"
  done
  touch -d @1700000001 "$releases/one"
  touch -d @1700000002 "$releases/two"
  touch -d @1700000003 "$releases/three"
  touch -d @1700000004 "$releases/four"
  ln -s releases/four "$link"
  install -d -m 0755 "$releases/keepme"
  printf 'a directory the tree does not recognise: no holosmith in it, so it is not a release\n' >"$releases/keepme/README"
  : >"$releases/stray.txt"

  # The unit a promotion writes: `ExecStart` through `current`, which is what makes a switch take effect at a restart.
  printf '[Unit]\nDescription=Holosmith server\n\n[Service]\nExecStart=%s/current/holosmith serve\nRestart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n' \
    "$tree" >"$unit"
  chmod 0644 "$unit"
  # The config `--root` reads: the **installed** paths, because `--root` prefixes them (`server deploy --root`'s rule),
  # and a hub role so the `doctor` runs ask no daemon question.
  printf 'owner = "%s"\nrole = "hub"\nbackend = "stub"\ncontrol_listen = "127.0.0.1:0"\npreview_listen = "127.0.0.1:0"\nmin_free_gb = 0\nsession_disk_soft_gb = 0\nidle_stop_min = 0\nkeep_releases = 2\n' \
    "$owner" >"$config"
  chmod 0644 "$config"

  # ---- 1. bullets 1 and 2: the shape, and the link is relative --------------------------------------------
  check "$name/link" "releases/four" "$(readlink "$link")"
  [ -f "$releases/four/holosmith" ] || fail "$name/release" "the live release holds no binary"
  line="$(grep '^ExecStart=' "$unit" || true)"
  check "$name/exec" "ExecStart=$tree/current/holosmith serve" "$line"
  if [ "$(grep -c '^ExecStart=' "$unit")" -eq 1 ]; then
    assertions=$((assertions + 1))
  else
    fail "$name/exec/one" "the unit has more than one ExecStart: $(cat "$unit")"
  fi

  # ---- 2. bullet 3: `version` reports the release directory the running file is in -----------------------
  # Inside a release; through `current` (which the kernel resolves, so the answer is the directory and not the link);
  # and from `dist/`, where there is no release to report and the line says so.
  check "$name/version/dir" "two" "$("$releases/two/holosmith" version | sed -n 's/^release //p')"
  check "$name/version/current" "four" "$("$link/holosmith" version | sed -n 's/^release //p')"
  check "$name/version/loose" "-" "$("$artifact" version | sed -n 's/^release //p')"
  "$releases/two/holosmith" version --json | grep -q '"release":"two"' ||
    fail "$name/version/json" "the json does not carry the release: $("$releases/two/holosmith" version --json)"
  assertions=$((assertions + 1))

  # ---- 3. bullet 5, first arm: the resolved path over a healthy tree -----------------------------------------
  status=0
  "$artifact" doctor --quick --config "$config" --root "$root" >"$WORK/$name.healthy.out" 2>&1 || status=$?
  check "$name/doctor/exit" 0 "$status"
  line="$(grep '^release: pass: ' "$WORK/$name.healthy.out" || true)"
  [ -n "$line" ] || fail "$name/doctor/pass" "a healthy tree is not a pass: $(cat "$WORK/$name.healthy.out")"
  resolved="$(readlink -f "$releases/four/holosmith")"
  case "$line" in
    *"current -> releases/four"*"$resolved"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/doctor/resolved" "the pass names neither the tag nor the resolved path ($resolved): $line" ;;
  esac

  # ---- 4. bullet 5, second arm: a link that names a directory which is gone ----------------------------------
  ln -sfn releases/gone "$link"
  status=0
  "$artifact" doctor --quick --config "$config" --root "$root" >"$WORK/$name.gone.out" 2>&1 || status=$?
  check "$name/gone/exit" 1 "$status"
  line="$(grep '^release: fail: ' "$WORK/$name.gone.out" || true)"
  [ -n "$line" ] || fail "$name/gone/fail" "a dangling link is not a failure: $(cat "$WORK/$name.gone.out")"
  case "$line" in
    *"releases/gone"*"ln -sfn releases/four"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/gone/sentence" "the failure names neither the state nor the fix: $line" ;;
  esac
  # The remedy names `four`, the newest *release* — the unrecognised `releases/keepme` has a newer mtime and is not one.
  case "$line" in
    *keepme*) fail "$name/gone/unrecognised" "the remedy offers a directory the tree does not recognise: $line" ;;
    *) assertions=$((assertions + 1)) ;;
  esac

  # ---- 5. `current` not a symlink at all ---------------------------------------------------------------------
  rm -f "$link"
  : >"$link"
  status=0
  "$artifact" doctor --quick --config "$config" --root "$root" >"$WORK/$name.file.out" 2>&1 || status=$?
  check "$name/file/exit" 1 "$status"
  case "$(grep '^release: fail: ' "$WORK/$name.file.out" || true)" in
    *"not a symlink"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/file/sentence" "a regular file named `current` is not a failure that says why: $(cat "$WORK/$name.file.out")" ;;
  esac
  rm -f "$link"

  # ---- 6. a machine that has promoted nothing: its unit names a binary, so a broken tree is not a refusal ----
  ln -sfn releases/gone "$link"
  printf '[Unit]\nDescription=Holosmith server\n\n[Service]\nExecStart=%s/bin/holosmith serve\n\n[Install]\nWantedBy=multi-user.target\n' \
    "$root" >"$unit"
  status=0
  "$artifact" doctor --quick --config "$config" --root "$root" >"$WORK/$name.plain.out" 2>&1 || status=$?
  check "$name/plain/exit" 0 "$status"
  case "$(grep '^release: pass: ' "$WORK/$name.plain.out" || true)" in
    *"$root/bin/holosmith"*"does not execute"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/plain/sentence" "the pass does not say what the unit runs instead: $(cat "$WORK/$name.plain.out")" ;;
  esac

  # ---- 7. nothing deployed: no unit and no tree, which is `unknown` and not a failure -----------------------
  status=0
  "$artifact" doctor --quick --config "$config" --root "$root/empty" >"$WORK/$name.none.out" 2>&1 || status=$?
  check "$name/none/exit" 0 "$status"
  case "$(grep '^release: ' "$WORK/$name.none.out" || true)" in
    'release: unknown: '*) assertions=$((assertions + 1)) ;;
    *) fail "$name/none/unknown" "a machine with no unit and no tree is not an honest unknown: $(cat "$WORK/$name.none.out")" ;;
  esac

  rm -rf "$root"
}

case_build() {
  local name=case_build root="$repo_root/target/verify-build-$$"
  local bin="$root/bin" out="$root/out" log="$root/docker.argv" compiled="$root/compiled"
  local built="$root/built" fake="$root/fake.bin" recipe="$root/recipe.txt"
  local host_target other_target image version tag digest answer file_line dynamic real="$root/real"
  case "$(uname -m)" in
    x86_64)
      host_target=x86_64-unknown-linux-musl
      other_target=aarch64-unknown-linux-musl
      ;;
    aarch64)
      host_target=aarch64-unknown-linux-musl
      other_target=x86_64-unknown-linux-musl
      ;;
    *) fail "$name/arch" "no release target for $(uname -m): docs/build.md names the two" ;;
  esac
  rm -rf "$root"
  mkdir -p "$bin" "$out"

  # The recipe's digest, computed the way `builder::recipe()` computes it: the Dockerfile, a `\n---\n` separator,
  # then the entry script. This is also the assertion that the shipped artifact's embedded recipe is this tree's —
  # a stale `dist/` would put a different value in the `--build-arg` below.
  cat "$repo_root/crates/holosmith/images/builder.Dockerfile" >"$recipe"
  printf '\n---\n' >>"$recipe"
  cat "$repo_root/crates/holosmith/images/builder-entry.sh" >>"$recipe"
  recipe="$(sha256sum "$recipe" | cut -d' ' -f1)"

  # The artifact the stand-in "builds" is this tree's own: a real static ELF for this machine, so the command's own
  # ELF check, the digest and the `.sha256` are measured over real bytes rather than over a placeholder.
  cp "$repo_root/dist/$host_target/holosmith" "$fake"

  cat >"$bin/docker" <<'SHIM'
#!/bin/sh
# A `docker` that records every argv and fakes the three questions a build asks: who the daemon is, whether the
# image is there, and what a build produces.
set -eu
{
  printf 'docker'
  for arg in "$@"; do printf ' %s' "$arg"; done
  printf '\n'
} >>"$HS_BUILD_LOG"
case "$1" in
  version)
    printf 'amd64\n'
    ;;
  image)
    # `image inspect --format …`: absent until the build below has "run", then this recipe's.
    [ -f "$HS_BUILD_BUILT" ] || exit 1
    printf '%s\n' "$HS_BUILD_RECIPE"
    ;;
  build)
    # The context is the archive on stdin; a real daemon reads it, and this one has to drain it.
    cat >/dev/null
    : >"$HS_BUILD_BUILT"
    printf 'faked the image build\n'
    ;;
  run)
    for arg in "$@"; do
      case "$arg" in
        *:/out) dir="${arg%:/out}" ;;
      esac
    done
    [ -n "${dir:-}" ] || exit 1
    cp "$HS_BUILD_FAKE" "$dir/.holosmith.new"
    if [ "${HS_BUILD_VARIANT:-1}" = 2 ]; then printf 'x' >>"$dir/.holosmith.new"; fi
    printf 'faked the artifact\n'
    ;;
  *)
    exit 1
    ;;
esac
SHIM
  chmod 0755 "$bin/docker"

  # The two compilers, which must never run on this host. They record and fail: if one is ever invoked the artifact
  # would be wrong as well as the rule broken, so a failure is the honest answer.
  local compiler
  for compiler in cargo rustc; do
    cat >"$bin/$compiler" <<'SHIM'
#!/bin/sh
printf '%s %s\n' "$0" "$*" >>"$HS_BUILD_COMPILED"
echo "$0: the host must not compile anything: the build runs in the pinned container" >&2
exit 1
SHIM
    chmod 0755 "$bin/$compiler"
  done

  # Run 1: the artifact is built, placed, digested and reported.
  answer="$(
    cd "$repo_root" &&
      PATH="$bin:$PATH" HS_BUILD_LOG="$log" HS_BUILD_COMPILED="$compiled" HS_BUILD_BUILT="$built" \
        HS_BUILD_RECIPE="$recipe" HS_BUILD_FAKE="$fake" \
        "$artifact" --config "$WORK/config.toml" build --target "$host_target" --out "$out" 2>&1
  )" || fail "$name/first" "the build command failed: $answer"
  [ -f "$out/holosmith" ] || fail "$name/artifact" "no artifact at $out/holosmith: $answer"
  cmp -s "$out/holosmith" "$fake" ||
    fail "$name/artifact" "the placed artifact is not the bytes the container wrote"
  digest="$(sha256sum "$out/holosmith" | cut -d' ' -f1)"
  [ "$(cat "$out/holosmith.sha256")" = "$digest  holosmith" ] ||
    fail "$name/digest" "$out/holosmith.sha256 does not hold '$digest  holosmith': $(cat "$out/holosmith.sha256" 2>/dev/null)"
  case "$answer" in
    *"build: artifact: $out/holosmith"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/report" "the command did not name the artifact it placed: $answer" ;;
  esac
  case "$answer" in
    *"build: sha256: $digest"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/report" "the command did not report the digest: $answer" ;;
  esac

  # The rule the story exists for: no compiler ran on this host.
  [ ! -e "$compiled" ] ||
    fail "$name/host-compiler" "a compiler ran on the host: $(cat "$compiled")"

  # The recorded argvs: the image build carries this recipe, and the run mounts the source read-only, the output
  # writable, hands over the tag, and names the triple.
  version="$(sed -n 's/^version //p' <<<"$("$artifact" version 2>/dev/null)")"
  image="holosmith-builder:$version"
  [ -n "$version" ] || fail "$name/version" "the artifact does not report a version"
  grep -q -- "--build-arg HOLOSMITH_BUILDER_RECIPE=$recipe" "$log" ||
    fail "$name/recipe" "the image build did not carry this tree's recipe: $(cat "$log")"
  grep -q -- " build -t $image " "$log" ||
    fail "$name/image" "the image build did not tag $image: $(cat "$log")"
  grep -q -- ":/src:ro" "$log" ||
    fail "$name/mount" "the source was not mounted read-only: $(cat "$log")"
  grep -q -- " -v $out:/out " "$log" ||
    fail "$name/mount" "the output directory was not the writable mount: $(cat "$log")"
  grep -q -- " -e HOLOSMITH_GIT_TAG=" "$log" ||
    fail "$name/tag" "the container was not handed the tree's tag: $(cat "$log")"
  grep -q -- " run --rm " "$log" ||
    fail "$name/run" "the build was not a one-off container: $(cat "$log")"
  tag="$(sed -n 's/.* -e HOLOSMITH_GIT_TAG=\([^ ]*\) .*/\1/p' "$log" | head -1)"
  [ -n "$tag" ] || fail "$name/tag" "no tag reached the container: $(cat "$log")"
  grep -q -- " $image $host_target\$" "$log" ||
    fail "$name/triple" "the container was not asked for $host_target: $(cat "$log")"

  # Run 2: the same tree again. The image is current (no second `build` in the log) and the artifact is already
  # these bytes, so nothing is written and the command says so.
  local builds_before builds_after
  builds_before="$(grep -c '^docker build ' "$log" || true)"
  answer="$(
    cd "$repo_root" &&
      PATH="$bin:$PATH" HS_BUILD_LOG="$log" HS_BUILD_COMPILED="$compiled" HS_BUILD_BUILT="$built" \
        HS_BUILD_RECIPE="$recipe" HS_BUILD_FAKE="$fake" \
        "$artifact" --config "$WORK/config.toml" build --target "$host_target" --out "$out" 2>&1
  )" || fail "$name/second" "the second build failed: $answer"
  builds_after="$(grep -c '^docker build ' "$log" || true)"
  check "$name/image-current" "$builds_before" "$builds_after"
  case "$answer" in
    *"is already this build"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/idempotent" "a re-run did not say the artifact was already this build: $answer" ;;
  esac

  # Run 3: a container that produces different bytes for the same release directory. Refused, and nothing replaced.
  if answer="$(
    cd "$repo_root" &&
      PATH="$bin:$PATH" HS_BUILD_LOG="$log" HS_BUILD_COMPILED="$compiled" HS_BUILD_BUILT="$built" \
        HS_BUILD_RECIPE="$recipe" HS_BUILD_FAKE="$fake" HS_BUILD_VARIANT=2 \
        "$artifact" --config "$WORK/config.toml" build --target "$host_target" --out "$out" 2>&1
  )"; then
    fail "$name/different" "a different binary was placed into a release directory that held one: $answer"
  fi
  case "$answer" in
    *"already holds a different binary"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/different" "the refusal did not say why: $answer" ;;
  esac
  [ "$(sha256sum "$out/holosmith" | cut -d' ' -f1)" = "$digest" ] ||
    fail "$name/different" "the refusal replaced the artifact"
  [ ! -e "$out/.holosmith.new" ] ||
    fail "$name/different" "the refused build's bytes were left behind"

  # Run 4: the container produced the wrong machine's binary (the stand-in copies this host's artifact). The
  # command reads the ELF header of what landed and refuses it, which is the acceptance's "builds for the
  # requested target" measured from the artifact rather than from the triple it was asked for.
  rm -rf "$root/wrong"
  mkdir -p "$root/wrong"
  if answer="$(
    cd "$repo_root" &&
      PATH="$bin:$PATH" HS_BUILD_LOG="$log" HS_BUILD_COMPILED="$compiled" HS_BUILD_BUILT="$built" \
        HS_BUILD_RECIPE="$recipe" HS_BUILD_FAKE="$fake" \
        "$artifact" --config "$WORK/config.toml" build --target "$other_target" --out "$root/wrong" 2>&1
  )"; then
    fail "$name/machine" "an artifact for another machine was accepted for $other_target: $answer"
  fi
  case "$answer" in
    *"was asked for"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/machine" "the refusal did not name the target: $answer" ;;
  esac
  [ ! -e "$root/wrong/holosmith" ] || fail "$name/machine" "a refused artifact was placed"
  [ ! -e "$compiled" ] || fail "$name/host-compiler" "a compiler ran on the host: $(cat "$compiled")"

  # Arm 2: the real container, when this machine can run one.
  if ! command -v docker >/dev/null 2>&1 || ! timeout 60 docker version >/dev/null 2>&1; then
    printf 'note %s: no Docker daemon answers, so the real container build was not measured (arm 1 was)\n' "$name"
    rm -rf "$root"
    return 0
  fi
  if ! timeout 60 docker image inspect "$image" >/dev/null 2>&1; then
    printf 'note %s: %s is not built on this machine yet, so the real build was not measured: run `holosmith build` once (arm 1 was)\n' \
      "$name" "$image"
    rm -rf "$root"
    return 0
  fi
  mkdir -p "$real"
  answer="$(
    cd "$repo_root" && timeout 3600 "$artifact" --config "$WORK/config.toml" build --target "$host_target" --out "$real" 2>&1
  )" || fail "$name/real" "the real container build failed: $(tail -20 <<<"$answer")"
  [ -f "$real/holosmith" ] || fail "$name/real" "the real build placed no artifact: $answer"
  file_line="$(file -b "$real/holosmith")"
  case "$file_line" in
    *"statically linked"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/real" "the real artifact is not statically linked: $file_line" ;;
  esac
  dynamic="$(readelf -d "$real/holosmith" 2>&1)"
  case "$dynamic" in
    *"There is no dynamic section in this file"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/real" "the real artifact has a dynamic section: $dynamic" ;;
  esac
  check "$name/real" "$(sha256sum "$real/holosmith" | cut -d' ' -f1)" \
    "$(cut -d' ' -f1 "$real/holosmith.sha256")"
  rm -rf "$root"
}

# The config `case_server_images` deploys: `deploy_config`'s shape with `agent_image` naming the tag step 3 builds.
# A second function rather than an argument to `deploy_config`, because that case deliberately names an image built
# by hand — which is what makes its step 3's remark about `agent_image` a fact of that machine.
images_config() {
  local path="$1" control="$2" preview="$3" version="$4"
  cat >"$path" <<EOF
owner = "$owner"
role = "server"
backend = "stub"
control_listen = "127.0.0.1:$control"
preview_listen = "127.0.0.1:$preview"
control_origin = "http://localhost:$control"
preview_origin = "http://localhost:$preview"
docker_host = "unix:///var/run/docker.sock"
agent_image = "agent-image:$version"
min_free_gb = 1
EOF
}

# image_proxy_ask <address> <port> <request line> [<header line>]: one request to a proxy running in a container,
# retried until it answers, printing its status line.
#
# A retry rather than a sleep, because a container's listener is up a moment after `docker run -d` returns and a
# first attempt that raced it would fail an assertion about the proxy with a sentence about a socket.
image_proxy_ask() {
  python3 - "$@" <<'PYASK'
import socket, sys, time

host, port, request = sys.argv[1], int(sys.argv[2]), sys.argv[3]
header = sys.argv[4] if len(sys.argv) > 4 and sys.argv[4] else ""
lines = [request] + ([header] if header else [])
head = ("\r\n".join(lines) + "\r\n\r\n").encode()
deadline = time.time() + 30
while True:
    try:
        with socket.create_connection((host, port), timeout=5) as sock:
            sock.sendall(head)
            answer = b""
            while True:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                answer += chunk
        print(answer.decode("utf-8", "replace").split("\r\n", 1)[0])
        break
    except OSError as error:
        if time.time() > deadline:
            print("no answer: %s" % error)
            break
        time.sleep(0.1)
PYASK
}

case_server_deploy() {
  local name=case_server_deploy root="$repo_root/target/verify-deploy-$$"
  local image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local control preview status line got entry n version
  local shims log state target count

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
    # A dry run reads step 6 (`tailscale serve status --json` is what the plan is made of), so the shim is on PATH
    # even for a run that installs nothing. Steps 5, 6 and 7 are this story's now, so nothing here is a skip.
    shims="$WORK/$name.shims"
    log="$WORK/$name.tailscale.log"
    state="$WORK/$name.tailscale.state"
    deploy_machine_shims "$shims" "$log" "$state"

    # 1. The dry run: the six steps in the spec's order, and an install root that does not appear.
    status=0
    PATH="$shims:$PATH" "$artifact" server deploy --dry-run --root "$root/install" \
      --config "$WORK/config-deploy-offline.toml" >"$WORK/$name.dry.out" 2>&1 || status=$?
    check "$name/dry/exit" 0 "$status"
    got="$(sed -n 's/^\([1-7]\)\. .*/\1/p' "$WORK/$name.dry.out" | tr -d '\n')"
    check "$name/dry/order" "123456" "$got"
    for entry in preflight files images proxies service health; do
      grep -q "^[1-6]\. $entry: \(needs\|satisfied\|skipped\|failed\): " "$WORK/$name.dry.out" ||
        fail "$name/dry/$entry" "no $entry line in the dry run: $(cat "$WORK/$name.dry.out")"
      assertions=$((assertions + 1))
    done
    grep -q '^2\. files: needs: ' "$WORK/$name.dry.out" ||
      fail "$name/dry/plan" "the dry run does not say what step 2 would do: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    # Step 3 is that story's, so the dry run plans it: two images to build, and neither is on this machine.
    grep -q '^3\. images: needs: ' "$WORK/$name.dry.out" ||
      fail "$name/dry/images" "the dry run does not plan step 3: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    # Steps 5 and 6 plan what they would do, and the sentence names the thing it would touch — the unit's path and
    # the three commands that are root's, and the wait step 6 is. A plan's content is on the step's own line and the
    # action lines under it (ADR-0085), so each check reads the whole block.
    line="$(deploy_block "$WORK/$name.dry.out" '^5\. service: needs: ')"
    [ -n "$line" ] || fail "$name/dry/service" "the dry run does not plan step 5: $(cat "$WORK/$name.dry.out")"
    case "$line" in
      *"$root/install/holosmith.service"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/dry/service/unit" "step 5 does not name the unit it would write: $line" ;;
    esac
    for command in "sudo install -m 0644" "sudo systemctl daemon-reload" "sudo systemctl enable --now holosmith.service"; do
      case "$line" in
        *"$command"*) assertions=$((assertions + 1)) ;;
        *) fail "$name/dry/service/command" "step 5 does not name '$command': $line" ;;
      esac
    done
    line="$(deploy_block "$WORK/$name.dry.out" '^6\. health: needs: ')"
    [ -n "$line" ] || fail "$name/dry/health" "the dry run does not plan step 6: $(cat "$WORK/$name.dry.out")"
    for target in "127.0.0.1:$control" '/api/health' 'doctor --full'; do
      case "$line" in
        *"$target"*) assertions=$((assertions + 1)) ;;
        *) fail "$name/dry/health/sentence" "step 6 does not name '$target': $line" ;;
      esac
    done
    # No step is a skip any more, and no line says another story owns one: `skipped` keeps its word in the five-word
    # vocabulary and loses its producer (ADR-0056), which is a fact about the report and not about a step's line.
    if grep -q ': skipped: ' "$WORK/$name.dry.out"; then
      fail "$name/dry/skipped" "a step is still a skip: $(grep ': skipped: ' "$WORK/$name.dry.out")"
    fi
    assertions=$((assertions + 1))
    if grep -q '^deploy: incomplete: ' "$WORK/$name.dry.out"; then
      fail "$name/dry/incomplete" "the report says a step belongs to a later story: $(cat "$WORK/$name.dry.out")"
    fi
    assertions=$((assertions + 1))
    [ ! -e "$root/install" ] || fail "$name/dry/root" "the dry run created $root/install"
    assertions=$((assertions + 1))
    grep -q '^deploy: 6 steps: ' "$WORK/$name.dry.out" ||
      fail "$name/dry/summary" "no summary line: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    grep -q '^deploy: into ' "$WORK/$name.dry.out" ||
      fail "$name/dry/into" "the report does not say which tree it describes: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    grep -qx "deploy: url: http://localhost:$control" "$WORK/$name.dry.out" ||
      fail "$name/dry/url" "the tail names no URL this machine will serve: $(cat "$WORK/$name.dry.out")"
    assertions=$((assertions + 1))
    grep -qx 'deploy: next: holosmith key set <provider>' "$WORK/$name.dry.out" ||
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

    # 2b. A failed **dry run** resumes as a dry run. The command that just ran changed nothing, so a resume that
    #     installed step 2 for real would be a different command than the one the operator asked for.
    status=0
    "$artifact" server deploy --dry-run --root "$root/absent-install" \
      --config "$WORK/config-deploy-absent.toml" >"$WORK/$name.absentdry.out" 2>&1 || status=$?
    check "$name/absentdry/exit" 1 "$status"
    line="$(tail -n 1 "$WORK/$name.absentdry.out")"
    case "$line" in
      'deploy: resume: holosmith server deploy '*--dry-run) assertions=$((assertions + 1)) ;;
      *) fail "$name/absentdry/resume" "a failed dry run's resume is not a dry run: $line" ;;
    esac
    case "$line" in
      *config-deploy-absent.toml*absent-install*) assertions=$((assertions + 1)) ;;
      *) fail "$name/absentdry/paths" "the resume does not name the paths it ran with: $line" ;;
    esac
    [ ! -e "$root/absent-install" ] || fail "$name/absentdry/root" "the dry run created $root/absent-install"
    assertions=$((assertions + 1))

    # 3. A run that gets past step 1 and then fails step 3: that same socket answers `_ping` and nothing a
    #    build's read needs, so step 2's files are on disk when the run stops and the machine is describable. The
    #    sentence is the daemon's own — a socket that cannot answer `docker image inspect` is the reason no image
    #    could be built — and it names the proxy image, which is the one step 4 would have needed.
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
    grep -q '^3\. images: failed: ' "$WORK/$name.partial.out" ||
      fail "$name/partial/step3" "step 3 did not fail against a `_ping` socket: $(cat "$WORK/$name.partial.out")"
    assertions=$((assertions + 1))
    grep -q "^3\. images: failed: .*holosmith-proxy:" "$WORK/$name.partial.out" ||
      fail "$name/partial/step3/image" "the failure does not name the proxy image: $(cat "$WORK/$name.partial.out")"
    assertions=$((assertions + 1))
    if grep -q '^4\. ' "$WORK/$name.partial.out"; then
      fail "$name/partial/stop" "a run that failed at step 3 evaluated step 4: $(cat "$WORK/$name.partial.out")"
    fi
    assertions=$((assertions + 1))
    [ -f "$root/partial/config.toml" ] ||
      fail "$name/partial/left" "the failed run left no config.toml, so the machine is not describable"
    assertions=$((assertions + 1))
    [ -d "$root/partial/data/secrets" ] ||
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
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s/online: no python3 to pick ports and answer `/api/health` (tests/deploy.rs covers the rest)\n' "$name"
    rm -rf "$root"
    return 0
  fi
  if ! command -v docker >/dev/null 2>&1 || ! docker version >/dev/null 2>&1 ||
    ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s/online: no docker daemon or no %s image (step 4 starts the proxies from the built proxy image; tests/deploy.rs covers the rest)\n' \
      "$name" "$image"
    rm -rf "$root"
    return 0
  fi
  # `hs_egress`/`hs_key` are the machine's names: a leftover from an earlier case would make this case's first
  # run read `satisfied`, so it takes them over and removes its own at the end.
  timeout 60 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  # Step 3 builds `agent-image:<version>` and `holosmith-proxy:<version>`, and this case asserts `changed`: it
  # removes both tags first, because `case_server_images` deliberately leaves them. The rebuild is layer-cached.
  version="$("$artifact" version | sed -n 's/^version //p')"
  [ -n "$version" ] || fail "$name/version" "the artifact does not report a version"
  timeout 120 docker image rm -f "holosmith-proxy:$version" "agent-image:$version" >/dev/null 2>&1 || true
  rm -rf "$root"
  mkdir -p "$root"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  # Both runs go through one shim directory, so the second run's step 6 is answered by what the first one wrote —
  # and its log is what says the second run applied nothing.
  shims="$WORK/$name.online-shims"
  log="$WORK/$name.online-tailscale.log"
  state="$WORK/$name.online-tailscale.state"
  rm -f "$log" "$state"
  deploy_machine_shims "$shims" "$log" "$state"
  # Step 6 waits for `/api/health`, and its in-process `doctor --full` reads a *held* control port as the serving
  # pass — so one responder on the config's control port is what makes `7. health: satisfied:` a measurement.
  # Nothing on this machine starts the unit (there is no systemd), and `cleanup()` kills this if a `fail` gets in
  # first. Step 1 stays `satisfied` with the port held: `preflight` remarks it rather than failing it, because
  # re-running a deploy on a serving machine is a promise the spec makes.
  holder_pid="$(health_responder "$control")"
  for _ in $(seq 1 100); do
    if (exec 3<>/dev/tcp/127.0.0.1/"$control") 2>/dev/null; then break; fi
    sleep 0.05
  done
  deploy_config "$WORK/config-deploy-online.toml" "$control" "$preview" "unix:///var/run/docker.sock" "$image"

  status=0
  PATH="$shims:$PATH" "$artifact" server deploy --root "$root/install" \
    --config "$WORK/config-deploy-online.toml" >"$WORK/$name.online.out" 2>&1 || status=$?
  check "$name/online/exit" 0 "$status"
  grep -q '^2\. files: changed: ' "$WORK/$name.online.out" ||
    fail "$name/online/step2" "step 2 did not create the files: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  grep -q '^3\. images: changed: ' "$WORK/$name.online.out" ||
    fail "$name/online/step3" "step 3 did not build the two images: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  grep -q '^4\. proxies: changed: ' "$WORK/$name.online.out" ||
    fail "$name/online/step4" "step 4 did not start the proxies: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  # The unit step, from the run that really wrote it: the file, and the commands that are root's as words.
  grep -q '^5\. service: changed: ' "$WORK/$name.online.out" ||
    fail "$name/online/step5" "step 5 did not write the unit: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  grep -q '^6\. health: satisfied: ' "$WORK/$name.online.out" ||
    fail "$name/online/step6" "step 6 did not find the machine serving: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  grep -q '^deploy: 6 steps: ' "$WORK/$name.online.out" ||
    fail "$name/online/summary" "no tally line: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  # The unit's bytes are the deliverable: `ExecStart` names this binary and the root's config, and no child was run
  # (ADR-0086 — `Tools` has no `systemctl` field at all, so this is a file's content and a log's silence).
  grep -q "^ExecStart=.* serve --config $root/install/config.toml$" "$root/install/holosmith.service" ||
    fail "$name/online/unit" "the unit does not run this binary against the root's config: $(cat "$root/install/holosmith.service")"
  assertions=$((assertions + 1))
  grep -q '^User=holosmith$' "$root/install/holosmith.service" ||
    fail "$name/online/unit/user" "the unit names no service user: $(cat "$root/install/holosmith.service")"
  assertions=$((assertions + 1))
  if [ -s "$log" ] && grep -q 'serve --bg' "$log"; then
    fail "$name/online/origins" "the deploy configured Tailscale origins, which left the deploy (ADR-0086): $(cat "$log")"
  fi
  assertions=$((assertions + 1))
  check "$name/online/config/mode" 644 "$(stat -c '%a' "$root/install/config.toml")"
  check "$name/online/data/mode" 755 "$(stat -c '%a' "$root/install/data")"
  check "$name/online/secrets/mode" 700 "$(stat -c '%a' "$root/install/data/secrets")"
  check "$name/online/keys/mode" 600 "$(stat -c '%a' "$root/install/data/secrets/keys.toml")"
  check "$name/online/root/mode" 700 "$(stat -c '%a' "$root/install")"
  # The file it wrote is the machine it measured: the prefixed data root, and a file that re-reads clean.
  grep -q "^data_root = \"$root/install/data\"$" "$root/install/config.toml" ||
    fail "$name/online/config/data_root" "the written config does not name the prefixed data root: $(cat "$root/install/config.toml")"
  assertions=$((assertions + 1))
  status=0
  "$artifact" config validate --config "$root/install/config.toml" >/dev/null 2>&1 || status=$?
  check "$name/online/config/valid" 0 "$status"
  docker ps --format '{{.Names}}' | grep -qx hs_egress ||
    fail "$name/online/egress" "hs_egress is not running"
  assertions=$((assertions + 1))
  docker ps --format '{{.Names}}' | grep -qx hs_key || fail "$name/online/key" "hs_key is not running"
  assertions=$((assertions + 1))
  docker network ls | grep -qw egress || fail "$name/online/network" "the egress network is not there"
  assertions=$((assertions + 1))
  grep -qx "deploy: url: http://localhost:$control" "$WORK/$name.online.out" ||
    fail "$name/online/url" "a completed run printed no URL this machine will serve: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))
  grep -qx 'deploy: next: holosmith key set <provider>' "$WORK/$name.online.out" ||
    fail "$name/online/next" "the tail names no key command: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))

  # The second run: the acceptance's "changes nothing", as a comparison of the tree, the config's bytes and the
  # two files the deploy must never rewrite (the provider keys and the VAPID pair).
  deploy_listing "$root/install" >"$WORK/$name.tree1"
  cp "$root/install/config.toml" "$WORK/$name.config1"
  cp "$root/install/data/secrets/keys.toml" "$WORK/$name.keys1"
  status=0
  PATH="$shims:$PATH" "$artifact" server deploy --root "$root/install" \
    --config "$WORK/config-deploy-online.toml" >"$WORK/$name.again.out" 2>&1 || status=$?
  check "$name/again/exit" 0 "$status"
  while read -r n entry; do
    grep -q "^$n\. $entry: satisfied: " "$WORK/$name.again.out" ||
      fail "$name/again/$n" "step $n is not satisfied on the second run: $(cat "$WORK/$name.again.out")"
    assertions=$((assertions + 1))
  done <<'AGAIN'
1 preflight
2 files
3 images
4 proxies
5 service
6 health
AGAIN
  deploy_listing "$root/install" >"$WORK/$name.tree2"
  cmp -s "$WORK/$name.tree1" "$WORK/$name.tree2" ||
    fail "$name/again/tree" "the second run changed the tree: $(diff "$WORK/$name.tree1" "$WORK/$name.tree2" 2>&1 | head -5)"
  assertions=$((assertions + 1))
  cmp -s "$WORK/$name.config1" "$root/install/config.toml" ||
    fail "$name/again/config" "the second run rewrote config.toml"
  assertions=$((assertions + 1))
  cmp -s "$WORK/$name.keys1" "$root/install/data/secrets/keys.toml" ||
    fail "$name/again/keys" "the second run rewrote keys.toml, which holds every stored provider key"
  assertions=$((assertions + 1))
  # Nothing was applied twice, which is what the second run's `satisfied` lines above say — and the deploy asked the
  # tailscale stand-in nothing at all any more (ADR-0086).
  if [ -s "$log" ] && grep -q 'serve --bg' "$log"; then
    fail "$name/again/origins" "the deploy configured Tailscale origins: $(cat "$log")"
  fi
  assertions=$((assertions + 1))
  timeout 60 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  kill "$holder_pid" 2>/dev/null || true
  wait "$holder_pid" 2>/dev/null || true
  holder_pid=""
  rm -rf "$root"
}

# the transport: the four `ssh` children, the two refusals that happen before anything is copied, and the
# target's own deploy report streaming back onto this command's stdout.
#
# **There is no ssh key on this machine and no second machine**, so "the target" is a stand-in plus a `--root` tree.
# The `ssh` here is a `sh` script that records every argv and runs the words after the host through `sh -c` — which
# is ssh's own contract rather than an approximation of it — with `$HOME` pointed at a directory this case owns, so
# the copy lands where the case can read it. Everything after that is real: the bytes are the shipped artifact's,
# the check is the machine's own `sha256sum`, and the deploy that follows is the artifact's own six steps.
#
# What no arm here can measure is a real ssh, a key, a second machine and a real `sudo` (there is none on this
# machine): `tests/remote.rs` covers all four argvs, every refusal and the `-t` rule, and `tests/deploy.rs` the
# `sudo`-prefixed children. This case measures the transport's *inside*: the copy, the digest, the arch refusal, a
# copy that does not verify, and that the target's lines really are this command's output.
#
# It needs `python3` (two ports, a `_ping` socket and one `/api/health` responder), the artifact, and the machine's
# `install`/`chmod`/`df`/`openssl`. It needs **no daemon and no image**: the server arm is a dry run over a `_ping`
# socket, and a hub builds and starts nothing.
case_deploy_ssh() {
  local name=case_deploy_ssh root="$repo_root/target/verify-ssh-$$"
  # `socket_pid`/`holder_pid` are assigned up front on purpose: a `local` with no value is **unset** in bash, and
  # the EXIT trap that runs after a `fail` in this function still sees its locals — so an unset `holder_pid` here
  # turns any failure into `line N: holder_pid: unbound variable` and buries the sentence that mattered.
  local control preview status line got shims sshlog log state home
  local socket_pid="" holder_pid=""
  local digest copy short hostarch other corrupt downloader
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 to pick the two ports, answer `_ping` and hold the control port (tests/remote.rs covers the four argvs and every refusal)\n' "$name"
    return 0
  fi
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
  hostarch="$(uname -m)"
  case "$hostarch" in
    x86_64|amd64) hostarch=x86_64; other="$repo_root/dist/aarch64-unknown-linux-musl/holosmith" ;;
    aarch64|arm64) hostarch=aarch64; other="$repo_root/dist/x86_64-unknown-linux-musl/holosmith" ;;
    *) fail "$name/arch" "this machine answers uname -m as $hostarch, which the deploy cannot name" ;;
  esac
  digest="$(sha256sum "$artifact" | cut -d' ' -f1)"
  [ -n "$digest" ] || fail "$name/digest" "the artifact has no digest"
  shims="$WORK/$name.shims"
  sshlog="$WORK/$name.ssh.log"
  log="$WORK/$name.tailscale.log"
  state="$WORK/$name.tailscale.state"
  home="$root/home"
  rm -f "$sshlog" "$log" "$state"
  mkdir -p "$home"
  deploy_machine_shims "$shims" "$log" "$state"
  deploy_ssh_shim "$shims"
  # The spec's "the CLI itself downloads nothing from the internet", as a program that must never be asked: if the
  # command fetched anything at all, one of these recorded the argv before it failed.
  mkdir -p "$WORK/$name.nodl"
  for downloader in curl wget; do
    printf '#!/bin/sh\nprintf "%%s\\n" "$0 $*" >>"%s"\nexit 1\n' "$WORK/$name.downloads.log" \
      >"$WORK/$name.nodl/$downloader"
    chmod 0755 "$WORK/$name.nodl/$downloader"
  done
  export HS_SSH_LOG="$sshlog" HS_SSH_HOME="$home"

  # ---- 1. the demo: a server deploy's dry run, executed on the target ------------------------------
  # Step 1 is a read, so the dry run performs it — against a `_ping` socket, which is what a machine with no daemon
  # can answer — and steps 2–6 plan. The six lines below are the **target's own**: they arrive through the fourth
  # child's inherited stdout, which is the acceptance's "streaming the output" and the demo's "lists steps
  # remotely" in one measurement.
  deploy_config "$WORK/config-deploy-ssh.toml" "$control" "$preview" "unix://$root/docker.sock" "pi-agent:local"
  status=0
  PATH="$WORK/$name.nodl:$shims:$PATH" "$artifact" server deploy --ssh me@box --dry-run --root "$root/tree" \
    --config "$WORK/config-deploy-ssh.toml" >"$WORK/$name.dry.out" 2>&1 || status=$?
  check "$name/dry/exit" 0 "$status"
  line="$(sed -n '1p' "$WORK/$name.dry.out")"
  case "$line" in
    "deploy: ssh: me@box is $hostarch; copied "*) assertions=$((assertions + 1)) ;;
    *) fail "$name/dry/sent" "the first line is not what was sent where: $line" ;;
  esac
  case "$line" in
    *"$digest"*) assertions=$((assertions + 1)) ;;
    *) fail "$name/dry/digest" "the line does not carry the artifact's digest: $line" ;;
  esac
  grep -q 'answered the same digest, so ~/.cache/holosmith/deploy-' "$WORK/$name.dry.out" ||
    fail "$name/dry/verified" "the copy was not reported as verified: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  grep -q '^deploy: ssh: the target will run: ~/.cache/holosmith/deploy-' "$WORK/$name.dry.out" ||
    fail "$name/dry/command" "the command the target runs is not printed: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  grep -qx 'deploy: dry run' "$WORK/$name.dry.out" ||
    fail "$name/dry/header" "the target's report does not call itself a dry run: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  got="$(sed -n 's/^\([1-7]\)\. .*/\1/p' "$WORK/$name.dry.out" | tr -d '\n')"
  check "$name/dry/order" "123456" "$got"
  grep -q '^deploy: into ' "$WORK/$name.dry.out" ||
    fail "$name/dry/into" "the target's report does not name its install tree: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  grep -q '^deploy: 6 steps: ' "$WORK/$name.dry.out" ||
    fail "$name/dry/tally" "the target's report has no tally: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  grep -qx "deploy: url: http://localhost:$control" "$WORK/$name.dry.out" ||
    fail "$name/dry/url" "the target's tail names no URL: $(cat "$WORK/$name.dry.out")"
  assertions=$((assertions + 1))
  [ ! -e "$root/tree" ] || fail "$name/dry/root" "the dry run created $root/tree on the target"
  assertions=$((assertions + 1))
  # The four children, as the argv the transport really built: the read, the copy, the check and the run.
  check "$name/dry/children" 4 "$(grep -c '' "$sshlog")"
  grep -q 'uname -m' "$sshlog" || fail "$name/dry/uname" "no architecture was read: $(cat "$sshlog")"
  assertions=$((assertions + 1))
  grep -q 'cat >' "$sshlog" || fail "$name/dry/copy" "no copy child ran: $(cat "$sshlog")"
  assertions=$((assertions + 1))
  grep -q 'sha256sum' "$sshlog" || fail "$name/dry/check" "no digest was taken on the target: $(cat "$sshlog")"
  assertions=$((assertions + 1))
  grep -q 'server deploy --config .* --root .*--dry-run' "$sshlog" ||
    fail "$name/dry/run" "the run child does not carry the operator's own flags: $(cat "$sshlog")"
  assertions=$((assertions + 1))
  # The copy is the artifact's bytes, executable, and named by the digest it was verified against.
  copy="$(find "$home/.cache/holosmith" -name 'deploy-*' -type f | head -n 1)"
  [ -n "$copy" ] || fail "$name/dry/copy/file" "the copy is not on the target"
  cmp -s "$copy" "$artifact" || fail "$name/dry/copy/bytes" "the copy is not the artifact's bytes"
  assertions=$((assertions + 1))
  check "$name/dry/copy/mode" 755 "$(stat -c '%a' "$copy")"
  short="${digest:0:12}"
  case "$copy" in
    *"/deploy-$short") assertions=$((assertions + 1)) ;;
    *) fail "$name/dry/copy/name" "the copy is not named by its digest's first twelve characters: $copy" ;;
  esac
  [ ! -s "$WORK/$name.downloads.log" ] || fail "$name/dry/download" "the command ran a downloader: $(cat "$WORK/$name.downloads.log")"
  assertions=$((assertions + 1))

  # ---- 2. a real install over the same transport: the hub's five steps, and a second run -------------
  # A hub is the half that needs no daemon and no image, so this arm applies steps 2, 5, 6 and 7 for real in a
  # `--root` tree the case owns — and the two skips prove the `Kind` travelled rather than being decided here.
  deploy_config "$WORK/config-deploy-ssh-hub.toml" "$control" "$preview" "unix:///var/run/docker.sock" "pi-agent:local" hub
  holder_pid="$(health_responder "$control")"
  for _ in $(seq 1 100); do
    if (exec 3<>/dev/tcp/127.0.0.1/"$control") 2>/dev/null; then break; fi
    sleep 0.05
  done
  : >"$sshlog"
  status=0
  PATH="$WORK/$name.nodl:$shims:$PATH" "$artifact" hub deploy --ssh me@box --root "$root/hub" \
    --config "$WORK/config-deploy-ssh-hub.toml" >"$WORK/$name.run.out" 2>&1 || status=$?
  check "$name/run/exit" 0 "$status"
  grep -q '^2\. files: changed: ' "$WORK/$name.run.out" ||
    fail "$name/run/files" "the target did not create the install tree: $(cat "$WORK/$name.run.out")"
  assertions=$((assertions + 1))
  for entry in images proxies; do
    grep -q "^[0-9]\. $entry: skipped: " "$WORK/$name.run.out" ||
      fail "$name/run/$entry" "step $entry is not a skip on the target, so the kind did not travel: $(cat "$WORK/$name.run.out")"
    assertions=$((assertions + 1))
  done
  for entry in service; do
    grep -q "^[0-9]\. $entry: changed: " "$WORK/$name.run.out" ||
      fail "$name/run/$entry" "step $entry was not applied on the target: $(cat "$WORK/$name.run.out")"
    assertions=$((assertions + 1))
  done
  # Step 6's word depends on whether the target's own `doctor --full` had anything to say *before* the run, so both
  # words are read: it waited and then measured, or it found the machine already serving.
  grep -q '^6\. health: \(changed\|satisfied\): ' "$WORK/$name.run.out" ||
    fail "$name/run/health" "step 6 is neither changed nor satisfied on the target: $(cat "$WORK/$name.run.out")"
  assertions=$((assertions + 1))
  # The target's own tail, streamed: its role, its URL and its next step. A hub has no key proxy, so `key set` is
  # not one of them.
  grep -qx 'deploy: role: hub' "$WORK/$name.run.out" ||
    fail "$name/run/role" "the target does not say it is a hub: $(cat "$WORK/$name.run.out")"
  assertions=$((assertions + 1))
  grep -qx 'deploy: next: holosmith hub host add <name> <url>' "$WORK/$name.run.out" ||
    fail "$name/run/next" "the target does not name a hub's next step: $(cat "$WORK/$name.run.out")"
  assertions=$((assertions + 1))
  # Exactly one URL line: the target's. This side prints no `deploy: url:` of its own, because it measured none.
  check "$name/run/url/once" 1 "$(grep -c '^deploy: url: ' "$WORK/$name.run.out")"
  grep -qx "role = \"hub\"" "$root/hub/config.toml" ||
    fail "$name/run/config/role" "the target's config is not the half that was installed: $(cat "$root/hub/config.toml")"
  assertions=$((assertions + 1))
  [ -f "$root/hub/holosmith.service" ] ||
    fail "$name/run/unit" "the target wrote no unit"
  assertions=$((assertions + 1))
  check "$name/run/children" 4 "$(grep -c '' "$sshlog")"
  status=0
  PATH="$WORK/$name.nodl:$shims:$PATH" "$artifact" hub deploy --ssh me@box --root "$root/hub" \
    --config "$WORK/config-deploy-ssh-hub.toml" >"$WORK/$name.again.out" 2>&1 || status=$?
  check "$name/again/exit" 0 "$status"
  grep -qx 'deploy: 6 steps: 4 satisfied, 0 changed, 2 skipped, 0 failed' "$WORK/$name.again.out" ||
    fail "$name/again/tally" "the second run on the target is not idempotent: $(cat "$WORK/$name.again.out")"
  assertions=$((assertions + 1))
  # The second run is the same four children, and it copied again — the transport is stateless — but the name is the
  # digest, so it overwrote the same file rather than leaving a second one behind.
  check "$name/again/children" 8 "$(grep -c '' "$sshlog")"
  check "$name/again/copies" 2 "$(grep -c 'cat >' "$sshlog")"
  check "$name/again/names" 1 "$(grep -o 'deploy-[0-9a-f]\{12\}' "$sshlog" | sort -u | grep -c '')"
  kill "$holder_pid" 2>/dev/null || true
  wait "$holder_pid" 2>/dev/null || true
  holder_pid=""

  # ---- 3. the architecture: refused before anything is copied, accepted when --binary matches ---------
  # The two musl artifacts `scripts/build.sh` leaves behind are the two builds this command can be given, so the
  # "other" one is a real ELF for a machine this one is not.
  home="$root/home-arch"
  mkdir -p "$home"
  export HS_SSH_HOME="$home"
  : >"$sshlog"
  if [ ! -x "$other" ]; then
    printf 'SKIP %s/arch: no %s artifact to offer as a mismatch (scripts/build.sh builds both; --no-build skips it)\n' \
      "$name" "$other"
  else
    status=0
    PATH="$WORK/$name.nodl:$shims:$PATH" "$artifact" server deploy --ssh me@box --binary "$other" --dry-run \
      --root "$root/arch" --config "$WORK/config-deploy-ssh.toml" >"$WORK/$name.arch.out" 2>&1 || status=$?
    check "$name/arch/exit" 1 "$status"
    grep -q 'cannot run there' "$WORK/$name.arch.out" ||
      fail "$name/arch/sentence" "the refusal does not say why: $(cat "$WORK/$name.arch.out")"
    assertions=$((assertions + 1))
    grep -q 'deploy: ssh: .*--binary.*' "$WORK/$name.arch.out" ||
      fail "$name/arch/advice" "the refusal names no way forward: $(cat "$WORK/$name.arch.out")"
    assertions=$((assertions + 1))
    # "before anything is copied": the read ran and nothing else did.
    check "$name/arch/children" 1 "$(grep -c '' "$sshlog")"
    [ ! -e "$home/.cache" ] || fail "$name/arch/copied" "an architecture mismatch copied the binary anyway"
    assertions=$((assertions + 1))
    grep -q '^deploy: resume: holosmith server deploy --ssh me@box --binary ' "$WORK/$name.arch.out" ||
      fail "$name/arch/resume" "the refusal prints no resume command: $(cat "$WORK/$name.arch.out")"
    assertions=$((assertions + 1))
  fi
  # …and the same target accepts a build for it, which is the acceptance's "unless `--binary` supplies a matching
  # build": the digest-named copy is this artifact, and the dry run runs from it.
  : >"$sshlog"
  status=0
  PATH="$WORK/$name.nodl:$shims:$PATH" "$artifact" server deploy --ssh me@box --binary "$artifact" --dry-run \
    --root "$root/arch-ok" --config "$WORK/config-deploy-ssh.toml" >"$WORK/$name.archok.out" 2>&1 || status=$?
  check "$name/archok/exit" 0 "$status"
  grep -q '^deploy: 6 steps: ' "$WORK/$name.archok.out" ||
    fail "$name/archok/report" "a matching --binary did not run the deploy: $(cat "$WORK/$name.archok.out")"
  assertions=$((assertions + 1))
  check "$name/archok/children" 4 "$(grep -c '' "$sshlog")"

  # ---- 4. a copy that does not verify aborts before anything executes ------------------------------
  # One byte more than was sent: the check is still the machine's own `sha256sum`, so the *bytes* are what is
  # measured rather than the transport's opinion of them.
  home="$root/home-corrupt"
  corrupt="$WORK/$name.corrupt"
  mkdir -p "$home" "$corrupt"
  export HS_SSH_HOME="$home"
  cat >"$corrupt/cat" <<CAT
#!$(command -v sh)
"$(command -v cat)" || exit 1
printf x
CAT
  chmod 0755 "$corrupt/cat"
  : >"$sshlog"
  status=0
  PATH="$corrupt:$shims:$PATH" "$artifact" server deploy --ssh me@box --dry-run --root "$root/corrupt" \
    --config "$WORK/config-deploy-ssh.toml" >"$WORK/$name.corrupt.out" 2>&1 || status=$?
  check "$name/corrupt/exit" 1 "$status"
  grep -q 'not the bytes that were sent' "$WORK/$name.corrupt.out" ||
    fail "$name/corrupt/sentence" "the refusal does not say the copy differs: $(cat "$WORK/$name.corrupt.out")"
  assertions=$((assertions + 1))
  grep -qF "$digest" "$WORK/$name.corrupt.out" ||
    fail "$name/corrupt/digest" "the refusal does not name the digest it wanted: $(cat "$WORK/$name.corrupt.out")"
  assertions=$((assertions + 1))
  # The copy and the check ran; the run did not. That is the acceptance's "aborts before executing anything".
  check "$name/corrupt/children" 3 "$(grep -c '' "$sshlog")"
  if grep -q 'server deploy' "$sshlog"; then
    fail "$name/corrupt/ran" "the deploy ran on the target over a copy that did not verify: $(cat "$sshlog")"
  fi
  assertions=$((assertions + 1))
  copy="$(find "$home/.cache/holosmith" -name 'deploy-*' -type f | head -n 1)"
  [ -n "$copy" ] || fail "$name/corrupt/file" "the corrupted copy is not on the target"
  check "$name/corrupt/size" "$(( $(stat -c '%s' "$artifact") + 1 ))" "$(stat -c '%s' "$copy")"
  # A failed transport line is an error, so the sentence is on stderr and the resume command on stdout — the local
  # deploy's shape for a failed run, one hop out.
  grep -q '^deploy: resume: holosmith server deploy --ssh me@box --config .* --root .*--dry-run' \
    "$WORK/$name.corrupt.out" ||
    fail "$name/corrupt/resume" "a failed remote run prints no local resume: $(cat "$WORK/$name.corrupt.out")"
  assertions=$((assertions + 1))

  # ---- 5. what clap and the host rule refuse, and what never ran ------------------------------------
  : >"$sshlog"
  status=0
  "$artifact" server deploy --binary "$artifact" >"$WORK/$name.nossh.out" 2>&1 || status=$?
  check "$name/nossh/exit" 2 "$status"
  grep -q -- '--ssh' "$WORK/$name.nossh.out" || fail "$name/nossh/sentence" "--binary without --ssh names no flag: $(cat "$WORK/$name.nossh.out")"
  assertions=$((assertions + 1))
  status=0
  "$artifact" server deploy --ssh=-oProxyCommand=curl --dry-run >"$WORK/$name.host.out" 2>&1 || status=$?
  check "$name/host/exit" 1 "$status"
  grep -q 'starts with `-`' "$WORK/$name.host.out" ||
    fail "$name/host/sentence" "an option smuggled through --ssh is not refused: $(cat "$WORK/$name.host.out")"
  assertions=$((assertions + 1))
  check "$name/host/children" 0 "$(grep -c '' "$sshlog")"
  [ ! -s "$WORK/$name.downloads.log" ] ||
    fail "$name/download" "the command ran a downloader: $(cat "$WORK/$name.downloads.log")"
  assertions=$((assertions + 1))

  # ---- 6. a run that fails on the target: its exit code, and the two streams kept apart --------------
  # The local side never reads the config (`--config` names the *target's* file), so a path that does not exist there
  # is the target's own refusal with its own exit code — which is the only way to measure that a child's non-zero
  # status becomes this command's, that the sentence it earns is on stderr, and that the local resume is the last
  # line on stdout. Its report has no step line, because the target's config load refuses before step 1.
  : >"$sshlog"
  status=0
  PATH="$WORK/$name.nodl:$shims:$PATH" "$artifact" server deploy --ssh me@box --dry-run \
    --config "$WORK/$name.no-such-config.toml" >"$WORK/$name.failed.out" 2>"$WORK/$name.failed.err" || status=$?
  check "$name/failed/exit" 1 "$status"
  # Four children ran, and the last of them was the deploy itself: the transport did its work and the target said no.
  check "$name/failed/children" 4 "$(grep -c '' "$sshlog")"
  grep -qx 'deploy: ssh: `holosmith server deploy` on me@box exited 1' "$WORK/$name.failed.out" ||
    fail "$name/failed/line" "the local side does not say how the target's run ended: $(cat "$WORK/$name.failed.out")"
  assertions=$((assertions + 1))
  check "$name/failed/resume" 1 "$(grep -c '^deploy: resume: holosmith server deploy --ssh me@box' "$WORK/$name.failed.out")"
  check "$name/failed/stderr" 0 "$(grep -c 'deploy: resume: ' "$WORK/$name.failed.err" || true)"
  # The target's own refusal arrived on stderr — the stream the run child inherited — and not on stdout.
  grep -q 'holosmith: server deploy:' "$WORK/$name.failed.err" ||
    fail "$name/failed/err" "the target's refusal is not on stderr: $(cat "$WORK/$name.failed.err")"
  assertions=$((assertions + 1))
  if grep -q '^1\. preflight' "$WORK/$name.failed.out"; then
    fail "$name/failed/report" "a run the target refused printed a step line: $(cat "$WORK/$name.failed.out")"
  fi
  assertions=$((assertions + 1))

  kill "$socket_pid" 2>/dev/null || true
  wait "$socket_pid" 2>/dev/null || true
  socket_pid=""
  unset HS_SSH_LOG HS_SSH_HOME
  rm -rf "$root"
}

# the machine steps from the shipped artifact: the VAPID pair and the unit `server deploy` renders, the three
# commands that are root's (as words on the step's line), and the `/api/health` wait that turns an install into "this
# machine is serving" — with `doctor`'s `perms` as the finding that makes a wrong mode a *startup* refusal, because the
# unit's `ExecStartPre` is `doctor --quick`.
#
# Two halves that skip independently:
#
#   **offline** builds the install tree by hand (`install -d`/`install -m`, so every mode is this case's own choice
#   rather than a umask's) and reads `perms` on it three times: a pass, a fail after `chmod 0755` on the secrets
#   directory — both modes and the fix in one sentence, and an exit code — and a pass once the mode is back. It then
#   dry-runs the deploy over a `_ping` fake: steps 5 and 6 are real work now, so the report plans what each would do
#   and nothing says `skipped`. It needs the artifact, `sh`/`install`/`chmod`, python3 and the shim — no daemon, no
#   image.
#   **online** applies the same six steps into its own `--root`, with the tailscale shim on PATH and a responder
#   holding the control port, and then runs the same command again: every step `satisfied`, the VAPID pair and the
#   unit byte-for-byte identical, and the tailscale shim's log **empty** (the deploy configures no origin any more,
#   ADR-0086). It needs the daemon and an image.
#
# What neither half can measure is recorded where it belongs: nothing here asks systemd to load the unit (there is no
# systemd on this machine), the origins are the operator's own step now, and the service user's commands and the
# `systemctl` trio are printed rather than run — `tests/deploy.rs`'s arms are what measure the words.
# `docs/build.md` is the by-hand procedure for the parts only a real host can show.
case_server_service() {
  local name=case_server_service root="$repo_root/target/verify-service-$$"
  local image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local control preview status line count absent shims log state target vapid unit serve_bg

  # ---- the offline half: `perms` on a hand-built tree, and the dry run's plan ----------------------
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s/offline: no python3 to pick ports and answer `_ping` (tests/doctor.rs owns the `perms` arms)\n' "$name"
  else
    rm -rf "$root"
    # The tree `server deploy` writes, made by hand. `perms` reads modes and owners; the two files' *contents* are
    # `vapid::is_pair`'s business — the deploy's read, not doctor's — so an empty keys.toml describes a deployed
    # machine as far as this check is concerned.
    install -d -m 0755 "$root" "$root/data" ||
      fail "$name/tree" "could not create the install tree under $root"
    install -d -m 0700 "$root/data/secrets" ||
      fail "$name/tree/secrets" "could not create the secrets directory"
    install -m 0600 /dev/null "$root/data/secrets/keys.toml" ||
      fail "$name/tree/keys" "could not create keys.toml"
    install -m 0600 /dev/null "$root/data/secrets/vapid.toml" ||
      fail "$name/tree/vapid" "could not create vapid.toml"
    control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
    preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
    socket_pid="$(fake_docker_ping "$root/docker.sock")"
    for _ in $(seq 1 100); do
      [ -S "$root/docker.sock" ] && break
      sleep 0.05
    done
    [ -S "$root/docker.sock" ] || fail "$name/socket" "the fake daemon socket never appeared"
    # The shim first: `doctor`'s tailscale finding and the dry run's step 6 both ask through the name on PATH, and a
    # machine whose own `tailscale` is logged out would otherwise make the pass below a failure that is not this
    # case's subject.
    shims="$WORK/$name.shims"
    log="$WORK/$name.tailscale.log"
    state="$WORK/$name.tailscale.state"
    deploy_machine_shims "$shims" "$log" "$state"
    # `--root "$root"` is the tree this case made: `doctor`'s four install paths are the root's (ADR-0086 D1),
    # so a run given only the file would answer about this machine's `<root>`, which on a machine that has
    # deployed for real is a directory owned by the service user. The config still carries `data_root`, because
    # `serve` and the dry run below read the file.
    deploy_config "$WORK/config-service-doctor.toml" "$control" "$preview" "unix://$root/docker.sock" "$image"
    printf 'data_root = "%s"\n' "$root/data" >>"$WORK/config-service-doctor.toml"

    # 1. The modes the deploy writes: one `perms` line, and the two files this case made are not among the paths the
    #    pass reports as ones it did not find.
    status=0
    PATH="$shims:$PATH" "$artifact" doctor --quick --config "$WORK/config-service-doctor.toml" --root "$root" \
      >"$WORK/$name.modes.out" 2>&1 || status=$?
    check "$name/modes/exit" 0 "$status"
    count="$(grep -c '^perms: pass: ' "$WORK/$name.modes.out" || true)"
    check "$name/modes/perms" 1 "$count"
    line="$(grep '^perms: pass: ' "$WORK/$name.modes.out" || true)"
    absent="$(printf '%s\n' "$line" | sed -n 's/.*not there yet: //p')"
    case "$absent" in
      *keys.toml* | *vapid.toml*)
        fail "$name/modes/made" "the pass reports a file this case made as one it did not find: $line"
        ;;
      *) assertions=$((assertions + 1)) ;;
    esac

    # 2. A wrong mode is the refusal `ExecStartPre=… doctor --quick` exists for: one sentence carrying both modes
    #    and the command that fixes it, and an exit code a boot would stop on.
    chmod 0755 "$root/data/secrets"
    status=0
    PATH="$shims:$PATH" "$artifact" doctor --quick --config "$WORK/config-service-doctor.toml" --root "$root" \
      >"$WORK/$name.mode.out" 2>&1 || status=$?
    check "$name/mode/exit" 1 "$status"
    line="$(grep '^perms: fail: ' "$WORK/$name.mode.out" || true)"
    [ -n "$line" ] || fail "$name/mode/perms" "a 0755 secrets directory is not a failure: $(cat "$WORK/$name.mode.out")"
    assertions=$((assertions + 1))
    for target in 0700 0755 'chmod 0700'; do
      case "$line" in
        *"$target"*) assertions=$((assertions + 1)) ;;
        *) fail "$name/mode/sentence" "the failure does not carry '$target': $line" ;;
      esac
    done

    # 3. The mode back, the answer back.
    chmod 0700 "$root/data/secrets"
    PATH="$shims:$PATH" "$artifact" doctor --quick --config "$WORK/config-service-doctor.toml" --root "$root" \
      >"$WORK/$name.restored.out" 2>&1 ||
      fail "$name/restored/exit" "doctor --quick still fails once the mode is back: $(cat "$WORK/$name.restored.out")"
    grep -q '^perms: pass: ' "$WORK/$name.restored.out" ||
      fail "$name/restored/perms" "the restored mode is not a pass: $(cat "$WORK/$name.restored.out")"
    assertions=$((assertions + 1))

    # 4. The dry run's plan for the three steps this story made real. Step 6 is a **read**, so a plan asks it: the
    #    shim is on PATH even for a run that installs nothing. As in `case_server_deploy`, a plan's content is the
    #    step's block and not its first line alone (ADR-0085).
    deploy_config "$WORK/config-service-dry.toml" "$control" "$preview" "unix://$root/docker.sock" "$image"
    status=0
    PATH="$shims:$PATH" "$artifact" server deploy --dry-run --root "$root/install" \
      --config "$WORK/config-service-dry.toml" >"$WORK/$name.dry.out" 2>&1 || status=$?
    check "$name/dry/exit" 0 "$status"
    line="$(deploy_block "$WORK/$name.dry.out" '^5\. service: needs: ')"
    [ -n "$line" ] || fail "$name/dry/service" "the dry run does not plan step 5: $(cat "$WORK/$name.dry.out")"
    case "$line" in
      *"$root/install/holosmith.service"*) assertions=$((assertions + 1)) ;;
      *) fail "$name/dry/service/unit" "step 5 does not name the unit it would write: $line" ;;
    esac
    line="$(deploy_block "$WORK/$name.dry.out" '^6\. health: needs: ')"
    [ -n "$line" ] || fail "$name/dry/health" "the dry run does not plan step 6: $(cat "$WORK/$name.dry.out")"
    for target in "127.0.0.1:$control" '/api/health' 'doctor --full'; do
      case "$line" in
        *"$target"*) assertions=$((assertions + 1)) ;;
        *) fail "$name/dry/health/sentence" "step 6 does not name '$target': $line" ;;
      esac
    done
    # 5. Neither a skip nor a story's remainder: steps 5 and 6 are real now, so a report that says otherwise
    #    carries the claim this story deleted (`skipped` keeps its word and loses its producer, ADR-0056).
    if grep -q ': skipped: ' "$WORK/$name.dry.out"; then
      fail "$name/dry/skipped" "a step is still a skip: $(grep ': skipped: ' "$WORK/$name.dry.out")"
    fi
    assertions=$((assertions + 1))
    if grep -q '^deploy: incomplete: ' "$WORK/$name.dry.out"; then
      fail "$name/dry/incomplete" "the report says a step belongs to a later story: $(cat "$WORK/$name.dry.out")"
    fi
    assertions=$((assertions + 1))
    # 6. A dry run creates nothing, including the install tree this case would otherwise have to clean up.
    [ ! -e "$root/install" ] || fail "$name/dry/root" "the dry run created $root/install"
    assertions=$((assertions + 1))

    kill "$socket_pid" 2>/dev/null || true
    wait "$socket_pid" 2>/dev/null || true
    socket_pid=""
  fi

  # ---- the online half: the six steps applied, and then the same command again --------------------
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s/online: no python3 to pick ports and answer `/api/health` (tests/deploy.rs covers the steps)\n' "$name"
    rm -rf "$root"
    return 0
  fi
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1 ||
    ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s/online: no docker daemon or no %s image (step 4 starts the proxies from the built proxy image; tests/deploy.rs covers the steps)\n' \
      "$name" "$image"
    rm -rf "$root"
    return 0
  fi
  # `hs_egress`/`hs_key` are the machine's names: a leftover from an earlier case would make this case's first run
  # read `satisfied`, so it takes them over and removes its own at the end.
  timeout 60 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  rm -rf "$root"
  mkdir -p "$root"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  shims="$WORK/$name.online-shims"
  log="$WORK/$name.online-tailscale.log"
  state="$WORK/$name.online-tailscale.state"
  rm -f "$log" "$state"
  deploy_machine_shims "$shims" "$log" "$state"
  # Step 6 waits for `/api/health`, and its in-process `doctor --full` reads a *held* control port as the serving
  # pass — so one responder on the config's control port is what makes `7. health: satisfied:` a measurement rather
  # than a 60-second wait. Nothing here starts the unit (there is no systemd), and `cleanup()` kills this if a `fail`
  # gets in first. Step 1 stays `satisfied` with the port held: `preflight` remarks a held port instead of failing
  # it, because re-running a deploy on a serving machine is a promise the spec makes.
  holder_pid="$(health_responder "$control")"
  for _ in $(seq 1 100); do
    if (exec 3<>/dev/tcp/127.0.0.1/"$control") 2>/dev/null; then break; fi
    sleep 0.05
  done
  deploy_config "$WORK/config-service-online.toml" "$control" "$preview" "unix:///var/run/docker.sock" "$image"

  status=0
  PATH="$shims:$PATH" "$artifact" server deploy --root "$root/install" \
    --config "$WORK/config-service-online.toml" >"$WORK/$name.online.out" 2>&1 || status=$?
  check "$name/online/exit" 0 "$status"
  for target in '^2\. files: changed: ' '^5\. service: changed: ' '^6\. health: satisfied: '; do
    grep -qE "$target" "$WORK/$name.online.out" ||
      fail "$name/online/steps" "the first run does not say '$target': $(cat "$WORK/$name.online.out")"
    assertions=$((assertions + 1))
  done
  grep -q '^deploy: 6 steps: ' "$WORK/$name.online.out" ||
    fail "$name/online/summary" "no tally line: $(cat "$WORK/$name.online.out")"
  assertions=$((assertions + 1))

  # The VAPID pair: mode 0600, and the two fields' own lengths are the format's proof — 65 bytes of uncompressed
  # P-256 point and 32 bytes of scalar, unpadded base64url, which is 87 and 43 characters. `vapid.rs`'s unit tests
  # own the DER walk and its vectors own these two lengths; what is measured here is the file the deploy installed.
  vapid="$root/install/data/secrets/vapid.toml"
  check "$name/vapid/mode" 600 "$(stat -c '%a' "$vapid")"
  grep -qE '^public_key = "[A-Za-z0-9_-]{87}"$' "$vapid" ||
    fail "$name/vapid/public" "public_key is not 87 base64url characters: $(cat "$vapid")"
  assertions=$((assertions + 1))
  grep -qE '^private_key = "[A-Za-z0-9_-]{43}"$' "$vapid" ||
    fail "$name/vapid/private" "private_key is not 43 base64url characters: $(cat "$vapid")"
  assertions=$((assertions + 1))

  # The unit: mode 0644 at `<root>/holosmith.service` — ADR-0086 puts the rendered unit in the root, and the
  # operator installs it from there — carrying the lines the acceptance names, the service user the deploy makes the
  # unit's, and the sandbox D3 promises. A gate with no root can check the bytes and not the confinement, which is
  # what D3 says the gate is for.
  unit="$root/install/holosmith.service"
  check "$name/unit/mode" 644 "$(stat -c '%a' "$unit")"
  for target in 'User=holosmith' 'Group=holosmith' 'UMask=0077' 'NoNewPrivileges=yes' 'PrivateTmp=yes' \
    'ProtectSystem=strict' "ReadWritePaths=$root/install" 'Restart=on-failure' \
    "ExecStartPre=$artifact doctor --quick" 'WantedBy=multi-user.target'; do
    grep -qxF "$target" "$unit" || fail "$name/unit/line" "the unit does not carry '$target': $(cat "$unit")"
    assertions=$((assertions + 1))
  done
  # `ProtectHome=yes` is written **except** under a root inside `/home`, where it would hide the root itself. This
  # tree lives under the repository, and this repository is under `/home`, so the gate is the arm that proves the
  # conditional — and `deploy::unit_text`'s own unit test is the arm that pins the directive itself.
  if [ "${root#/home}" = "$root" ]; then
    grep -qxF 'ProtectHome=yes' "$unit" ||
      fail "$name/unit/protect" "the unit hides /home from a root that is not under it: $(cat "$unit")"
    assertions=$((assertions + 1))
  elif grep -q '^ProtectHome=' "$unit"; then
    fail "$name/unit/protect" "a root under /home is hidden from itself: $(cat "$unit")"
  else
    assertions=$((assertions + 1))
  fi
  # The program is the one that wrote the unit (`ExecStart` names it because a `PATH` lookup is not the binary that
  # ran), and the config is the root's — so the two are asserted by shape, the way `case_server_deploy` does.
  grep -q "^ExecStart=.* serve --config $root/install/config.toml$" "$unit" ||
    fail "$name/unit/exec" "the unit does not run this binary against the root's config: $(cat "$unit")"
  assertions=$((assertions + 1))

  # Tailscale left the deploy (ADR-0086): the stand-in on `PATH` is what would have been called, so its log is the
  # measurement that nothing was.
  if [ -s "$log" ] && grep -q 'serve --bg' "$log"; then
    fail "$name/origins" "the deploy configured Tailscale origins: $(cat "$log")"
  fi
  assertions=$((assertions + 1))

  # The second run: every step `satisfied`, the two files byte-identical, and no second `serve --bg` — the read
  # answered, so nothing was applied twice. That is the acceptance's "changes nothing", for the steps that make it
  # expensive to get wrong.
  cp "$vapid" "$WORK/$name.vapid1"
  cp "$unit" "$WORK/$name.unit1"
  status=0
  PATH="$shims:$PATH" "$artifact" server deploy --root "$root/install" \
    --config "$WORK/config-service-online.toml" >"$WORK/$name.again.out" 2>&1 || status=$?
  check "$name/again/exit" 0 "$status"
  while read -r n entry; do
    grep -q "^$n\. $entry: satisfied: " "$WORK/$name.again.out" ||
      fail "$name/again/$n" "step $n is not satisfied on the second run: $(cat "$WORK/$name.again.out")"
    assertions=$((assertions + 1))
  done <<'AGAIN'
1 preflight
2 files
3 images
4 proxies
5 service
6 health
AGAIN
  cmp -s "$WORK/$name.vapid1" "$vapid" ||
    fail "$name/again/vapid" "the second run rewrote the VAPID pair, which invalidates every existing push subscription"
  assertions=$((assertions + 1))
  cmp -s "$WORK/$name.unit1" "$unit" || fail "$name/again/unit" "the second run rewrote the unit"
  assertions=$((assertions + 1))
  if [ -s "$log" ] && grep -q 'serve --bg' "$log"; then
    fail "$name/again/origins" "the second run configured Tailscale origins: $(cat "$log")"
  fi
  assertions=$((assertions + 1))

  kill "$holder_pid" 2>/dev/null || true
  wait "$holder_pid" 2>/dev/null || true
  holder_pid=""
  timeout 60 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  rm -rf "$root"
}

# the `--full` from the shipped artifact, at **item 10 §7's moment**: the machine is already serving, which
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
  # Two `local`s, not one: `set -u` expands `$root` before `local` assigns it (hit again).
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
  # `install -d -m 0755` for the data root rather than `mkdir -p`: `perms` compares it against the 0755 `server
  # deploy` writes, and this shell's umask makes `mkdir -p` write 0775 — the fixture has to be the machine the
  # deploy leaves, not this shell's umask.
  install -d -m 0755 "$data" || fail "$name/mkdir" "could not create $data"
  mkdir -p "$root/projects"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  # The scratch lives under `target/` for ADR-0035's reason: the daemon resolves a mount's source in the host's
  # namespace, and the proxies mount this binary at its own path. **The two listeners are in the file**, because
  # `serve --port 0` would leave the config naming a port nothing listens on; `--root "$root"` is what points
  # `doctor` at this case's tree, its four install paths being the root's (ADR-0086 D1).
  printf 'owner = "%s"\nrole = "server"\nbackend = "docker"\nmax_live_sessions = 1\nprojects_root = "%s"\nagent_image = "%s"\ndata_root = "%s"\ncontrol_listen = "127.0.0.1:%s"\npreview_listen = "127.0.0.1:%s"\nmin_free_gb = 1\nidle_stop_min = 0\n' \
    "$owner" "$root/projects" "$image" "$data" "$control" "$preview" >"$WORK/config-doctor-full.toml"
  # Its own boot, **on the config's own ports**: `start_serve` always passes `--port 0`, which would leave the
  # config naming a port nothing listens on and `doctor`'s port check asking about the wrong address. The banner
  # is what says serve is up (the proxies and the boot's self-test run before it), and the loop gives the
  # self-test's container the half minute it can take.
  serve_err="$WORK/$name.serve.err"
  "$artifact" serve --config "$WORK/config-doctor-full.toml" --data-dir "$data" \
    --admin-dir "$WORK/data/run" \
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

  "$artifact" doctor --full --config "$WORK/config-doctor-full.toml" --root "$root" \
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
    # of failing an assertion (measured, with 's two-megabyte tool output).
    expect_no_cors "$label" "$(sed '/^\r$/q' "$file")"
  done
}

# --------------------------------------------------------------------------------------------------------------
# the three commands, from the shipped artifact: `status`, `logs`, `uninstall`.
# --------------------------------------------------------------------------------------------------------------

# The one command that describes a machine, one `key value` line per fact (spec's Commands table, ADR-0059).
#
# It boots a `serve` of **its own**, and that is load-bearing rather than tidy: `start_serve` always passes
# `--port 0`, while `status` asks the address the *config* names — so against the shared boot every uptime line
# would be `unknown:` and this case would be measuring the arm it did not come for. The config therefore names both
# the listener and the origin, the banner is what proves they agree, and the shared boot is put back afterwards
# because the cases after this one talk to `$port`.
#
# What is measured, each from the reader that owns it: `role` and `version` against the endpoint's own answer,
# `uptime` against its `uptime_s`, the six `sessions <status>` lines against the sessions list's own length (and
# `idle` moving by one when a session is created), both `proxy` lines against the daemon's answer, and the
# `machines` line naming — the story that will build the registry it cannot read yet.
case_status() {
  local name=case_status root="$repo_root/target/verify-status-$$"
  local control preview status body role_word uptime listed total line proxy state id
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'

  if [ -n "$serve_pid" ] && kill -0 "$serve_pid" 2>/dev/null; then
    stop_serve
  fi
  rm -rf "$root"
  mkdir -p "$root"
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  # `data_root`, both listeners and the control origin are in the *file*, because `status` reads the config and
  # nothing else (`--port` and `--data-dir` are `serve` overrides).
  printf 'owner = "%s"\nrole = "server"\nbackend = "stub"\ndata_root = "%s"\ncontrol_listen = "127.0.0.1:%s"\ncontrol_origin = "http://127.0.0.1:%s"\npreview_listen = "127.0.0.1:%s"\nidle_stop_min = 0\nmin_free_gb = 1\n' \
    "$owner" "$root/data" "$control" "$control" "$preview" >"$WORK/config-status.toml"
  serve_err="$WORK/serve-status.err"
  "$artifact" serve --config "$WORK/config-status.toml" --data-dir "$root/data" \
    --admin-dir "$WORK/data/run" \
    >"$WORK/$name.serve.out" 2>"$serve_err" &
  serve_pid=$!
  for _ in $(seq 1 400); do
    grep -q '^control_listen ' "$serve_err" 2>/dev/null && break
    kill -0 "$serve_pid" 2>/dev/null || break
    sleep 0.05
  done
  grep -q "^control_listen 127.0.0.1:$control\$" "$serve_err" ||
    fail "$name/serve" "serve did not bind the config's control port: $(cat "$serve_err")"
  assertions=$((assertions + 1))
  # The globals the fetch helpers use: this boot's own port and origin, not the shared ones.
  port="$control"
  control_listen="127.0.0.1:$control"
  control_origin="http://127.0.0.1:$control"
  preview_port="$preview"
  preview_listen="127.0.0.1:$preview"

  fetch "$name-health" GET /api/health "Host: 127.0.0.1" "Origin: $control_origin" ||
    fail "$name/health" "GET /api/health did not answer"
  body="$(cat "$WORK/responses/$name-health.body")"
  role_word="$(printf '%s\n' "$body" | sed -n 's/.*"role":"\([a-z]*\)".*/\1/p')"
  uptime="$(printf '%s\n' "$body" | sed -n 's/.*"uptime_s":\([0-9]*\).*/\1/p')"
  [ -n "$role_word" ] || fail "$name/health" "the health answer names no role: $body"
  [ -n "$uptime" ] || fail "$name/health" "the health answer carries no uptime: $body"

  status=0
  "$artifact" status --config "$WORK/config-status.toml" >"$WORK/$name.out" 2>&1 || status=$?
  check "$name/exit" 0 "$status"
  # `role` and `version` are the machine's own two facts: the config's role and this binary's version.
  grep -qx "role $role_word" "$WORK/$name.out" ||
    fail "$name/role" "the role line is not the machine's role ($role_word): $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  grep -qx "version $version" "$WORK/$name.out" ||
    fail "$name/version" "the version line is not this artifact's: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # `uptime` is the endpoint's own number: the two runs are seconds apart, so it may have moved by a second or two
  # and may not have gone backwards.
  grep -qxE "uptime ($uptime|$((uptime + 1))|$((uptime + 2)))s" "$WORK/$name.out" ||
    fail "$name/uptime" "the uptime line is not the endpoint's $uptime: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  # One line per status, in the enum's own words, and none of them invented. The *sum* is checked below, against
  # the sessions list, where there is something to compare it with — summing here measured nothing.
  for word in starting idle running stopped error deleting; do
    grep -qE "^sessions $word [0-9]+$" "$WORK/$name.out" ||
      fail "$name/sessions" "no \`sessions $word N\` line: $(cat "$WORK/$name.out")"
    assertions=$((assertions + 1))
  done
  # Both proxy lines, each the daemon's own answer.
  for proxy in hs_egress hs_key; do
    state=""
    if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
      state="$(docker inspect --format '{{.State.Running}}' "$proxy" 2>/dev/null || true)"
    fi
    if [ "$state" = true ]; then
      grep -qx "proxy $proxy running" "$WORK/$name.out" ||
        fail "$name/proxy" "$proxy is running and status does not say so: $(cat "$WORK/$name.out")"
    else
      grep -qx "proxy $proxy not running" "$WORK/$name.out" ||
        fail "$name/proxy" "$proxy is not running and status does not say so: $(cat "$WORK/$name.out")"
    fi
    assertions=$((assertions + 1))
  done
  # The registry: this boot's data root has no `hosts.toml` and nothing has registered, so the fact is a
  # **count of zero** and not the phase-2 sentence that used to name. The rows themselves are
  # `case_hub_deploy`'s, against a machine that really has some.
  grep -qx 'machines 0' "$WORK/$name.out" ||
    fail "$name/machines" "an empty registry is not a count of zero: $(cat "$WORK/$name.out")"
  assertions=$((assertions + 1))
  if grep -q 'NO_REGISTRY\|no hub has registered' "$WORK/$name.out"; then
    fail "$name/machines/stale" "status still prints the phase-2 sentence: $(cat "$WORK/$name.out")"
  fi
  assertions=$((assertions + 1))

  # A session moves one count, and the six counts are the table's own sum: `1 idle` is what a create leaves, and
  # the list's length is the independent reader of the same rows.
  fetch_body "$name-create" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-status-1" ||
    fail "$name/create" "POST /api/sessions did not answer"
  expect_status "$name/create" "$(answer "$name-create")" 202
  id="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/$name-create.body" | sed -n '1p')"
  [ -n "$id" ] || fail "$name/create" "the create names no id: $(cat "$WORK/responses/$name-create.body")"
  wait_for_idle_id "$name-idle" "$id"

  # Two seconds later, so "the uptime moved" is a fact about the clock rather than about rounding: a `status` that
  # printed `0s` would satisfy "well-formed" and fail this.
  sleep 2
  status=0
  "$artifact" status --config "$WORK/config-status.toml" >"$WORK/$name.after.out" 2>&1 || status=$?
  check "$name/after/exit" 0 "$status"
  grep -qx 'sessions idle 1' "$WORK/$name.after.out" ||
    fail "$name/after/idle" "a created session is not counted as idle: $(cat "$WORK/$name.after.out")"
  assertions=$((assertions + 1))
  line="$(sed -n 's/^uptime \([0-9][0-9]*\)s$/\1/p' "$WORK/$name.after.out")"
  [ -n "$line" ] || fail "$name/after/uptime" "the uptime line is not a measurement: $(cat "$WORK/$name.after.out")"
  [ "$line" -ge "$((uptime + 2))" ] ||
    fail "$name/after/uptime_moved" "the uptime was $uptime earlier and is $line now, two seconds later"
  assertions=$((assertions + 1))
  total=0
  for word in starting idle running stopped error deleting; do
    line="$(grep -E "^sessions $word [0-9]+$" "$WORK/$name.after.out" || true)"
    [ -n "$line" ] || fail "$name/after/sessions" "no \`sessions $word N\` line: $(cat "$WORK/$name.after.out")"
    total=$((total + ${line##* }))
  done
  fetch "$name-list" GET /api/sessions "Host: 127.0.0.1" "Origin: $control_origin" ||
    fail "$name/list" "GET /api/sessions did not answer"
  listed="$(grep -o '"id":"[0-9a-f]\{16\}"' "$WORK/responses/$name-list.body" | wc -l | tr -d ' ')"
  check "$name/after/total" "$listed" "$total"

  # The one arm that changes the exit code: a database nobody can read is a `fail:` line and exit 1, not six
  # measured zeros. A file where the database should be is the cheapest way to make SQLite refuse.
  mkdir -p "$root/broken"
  printf 'this is not a database' >"$root/broken/app.db"
  printf 'owner = "%s"\nrole = "server"\nbackend = "stub"\ndata_root = "%s"\ncontrol_listen = "127.0.0.1:%s"\nmin_free_gb = 1\n' \
    "$owner" "$root/broken" "$control" >"$WORK/config-status-broken.toml"
  status=0
  "$artifact" status --config "$WORK/config-status-broken.toml" >"$WORK/$name.broken.out" 2>&1 || status=$?
  check "$name/broken/exit" 1 "$status"
  grep -q '^sessions fail: ' "$WORK/$name.broken.out" ||
    fail "$name/broken/line" "an unreadable database is not a failed read: $(cat "$WORK/$name.broken.out")"
  assertions=$((assertions + 1))
  if grep -q '^sessions idle ' "$WORK/$name.broken.out"; then
    fail "$name/broken/zeros" "the counts were printed over a read that failed: $(cat "$WORK/$name.broken.out")"
  fi
  assertions=$((assertions + 1))

  # What this case made it takes back: its own data root dies with the directory, and the shared boot goes back
  # for the cases after this one.
  rm -rf "$root"
  stop_serve
  start_serve
}

# The two log sources, and the one child each needs (spec's Commands table, ADR-0059).
#
# No server and no daemon: the journald arm is a stand-in on `PATH` — the command names `journalctl`, exactly as a
# machine does — and the session arm is a file this case wrote at the path `exit::SessionLog` owns. Nothing here
# needs python3 either, so the case has no `SKIP` line.
#
# What a shell can measure about `-f` is that the flag reaches the child; that a followed **file** prints what
# arrives is `logs.rs`'s unit test (a line appended after the first read is printed), because the alternative here
# would be a sleep and a race.
case_logs() {
  local name=case_logs root="$repo_root/target/verify-logs-$$"
  local shims status log line

  rm -rf "$root"
  shims="$root/shims"
  mkdir -p "$shims" "$root/data/sessions/0123456789abcdef"
  export HS_JOURNAL_LOG="$root/journal.log"
  cat >"$shims/journalctl" <<'SHIM'
#!/bin/sh
# The gate's journald: records the argv, then prints what a unit's log looks like — or refuses, when the case asks
# it to. `-f` is recorded and not honoured, so a following run exits instead of holding the case open.
printf 'journalctl %s\n' "$*" >>"$HS_JOURNAL_LOG"
if [ -n "${HS_JOURNAL_FAIL:-}" ]; then
  printf 'journalctl: no journal files were found\n' >&2
  exit 3
fi
printf 'holosmith[42]: control_listen 127.0.0.1:8080\nholosmith[42]: serving\n'
SHIM
  chmod 0755 "$shims/journalctl"
  log="$root/data/sessions/0123456789abcdef/session.log"
  printf 'launch: pi --mode rpc\nexit 0\n' >"$log"
  printf 'owner = "%s"\nbackend = "stub"\ndata_root = "%s"\nmin_free_gb = 1\n' \
    "$owner" "$root/data" >"$WORK/config-logs.toml"

  # The server's log: one child, inherited stdout, and its own exit code.
  status=0
  PATH="$shims:$PATH" "$artifact" logs --config "$WORK/config-logs.toml" \
    >"$root/server.out" 2>"$root/server.err" || status=$?
  check "$name/server/exit" 0 "$status"
  grep -qx 'holosmith\[42\]: serving' "$root/server.out" ||
    fail "$name/server/out" "the child's log did not reach stdout: $(cat "$root/server.out" "$root/server.err")"
  assertions=$((assertions + 1))
  check "$name/server/argv" "journalctl -u holosmith.service" "$(cat "$HS_JOURNAL_LOG")"

  # `-f` is the child's flag: `journalctl -u holosmith.service -f`, nothing else.
  : >"$HS_JOURNAL_LOG"
  status=0
  PATH="$shims:$PATH" "$artifact" logs -f --config "$WORK/config-logs.toml" \
    >"$root/follow.out" 2>&1 || status=$?
  check "$name/follow/exit" 0 "$status"
  check "$name/follow/argv" "journalctl -u holosmith.service -f" "$(cat "$HS_JOURNAL_LOG")"

  # A journald that refuses is its own answer and its own code — the command adds one sentence, not a verdict.
  : >"$HS_JOURNAL_LOG"
  status=0
  HS_JOURNAL_FAIL=1 PATH="$shims:$PATH" "$artifact" logs --config "$WORK/config-logs.toml" \
    >"$root/failing.out" 2>"$root/failing.err" || status=$?
  check "$name/journald/exit" 3 "$status"
  grep -q 'no journal files were found' "$root/failing.err" ||
    fail "$name/journald/sentence" "the child's own line did not reach stderr: $(cat "$root/failing.err")"
  assertions=$((assertions + 1))

  # A session's log, byte for byte, and no child asked for it.
  : >"$HS_JOURNAL_LOG"
  status=0
  PATH="$shims:$PATH" "$artifact" logs --session 0123456789abcdef --config "$WORK/config-logs.toml" \
    >"$root/session.out" 2>"$root/session.err" || status=$?
  check "$name/session/exit" 0 "$status"
  cmp -s "$log" "$root/session.out" ||
    fail "$name/session/bytes" "the session's log was not printed as written: $(cat "$root/session.out" "$root/session.err")"
  assertions=$((assertions + 1))
  [ ! -s "$HS_JOURNAL_LOG" ] ||
    fail "$name/session/child" "the session arm asked journald: $(cat "$HS_JOURNAL_LOG")"
  assertions=$((assertions + 1))

  # Following a file prints it and keeps following: `timeout` kills the loop, and the exit code is its own 124.
  status=0
  timeout 2 env PATH="$shims:$PATH" "$artifact" logs -f --session 0123456789abcdef \
    --config "$WORK/config-logs.toml" >"$root/session-follow.out" 2>&1 || status=$?
  check "$name/session/follow/exit" 124 "$status"
  grep -qx 'launch: pi --mode rpc' "$root/session-follow.out" ||
    fail "$name/session/follow/out" "the followed log printed nothing: $(cat "$root/session-follow.out")"
  assertions=$((assertions + 1))

  # Two refusals, each one sentence: an id that is not one (checked before it becomes a path), and one this machine
  # has written no log for.
  status=0
  PATH="$shims:$PATH" "$artifact" logs --session ../../etc/passwd --config "$WORK/config-logs.toml" \
    >"$root/bad-id.out" 2>&1 || status=$?
  check "$name/bad_id/exit" 1 "$status"
  grep -q 'is not a session id' "$root/bad-id.out" ||
    fail "$name/bad_id/sentence" "the refusal does not say what is wrong: $(cat "$root/bad-id.out")"
  assertions=$((assertions + 1))
  status=0
  PATH="$shims:$PATH" "$artifact" logs --session ffffffffffffffff --config "$WORK/config-logs.toml" \
    >"$root/no-log.out" 2>&1 || status=$?
  check "$name/no_log/exit" 1 "$status"
  grep -q 'session.log' "$root/no-log.out" ||
    fail "$name/no_log/sentence" "the refusal names no path: $(cat "$root/no-log.out")"
  assertions=$((assertions + 1))

  unset HS_JOURNAL_LOG
  rm -rf "$root"
}

# The teardown: the unit, the two proxies and the network go, the data stays, and a second run says `nothing`
# (spec's Commands table, ADR-0059).
#
# **A daemon is this case's premise**, because the command removes the machine's `egress` network and asks the daemon
# what is there — a teardown with no daemon is not a teardown this case can measure, and `tests/uninstall.rs` covers
# the same four words over a fake machine on every `cargo test` run, so the SKIP line does not hide the subject.
# The machine's own network is **restored** at the end: `tests/container.rs`'s egress test needs it, and a case that
# left the machine worse than it found it is the orphan a later change owns.
#
# The refusal's live row comes from a boot of its own over the tree's data root, which is the only way to put a row
# in a database this case is about to delete without writing SQL by hand.
case_uninstall() {
  local name=case_uninstall root="$repo_root/target/verify-uninstall-$$"
  local image="${HOLOSMITH_TEST_IMAGE:-pi-agent:local}"
  local tree status line id networks egress_before control preview
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'

  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    printf 'SKIP %s: no Docker daemon answers, and this command removes the machine network (tests/uninstall.rs covers the words)\n' "$name"
    return 0
  fi
  rm -rf "$root"
  mkdir -p "$root"
  printf 'owner = "%s"\nrole = "server"\nbackend = "stub"\nmin_free_gb = 1\n' \
    "$owner" >"$WORK/config-uninstall.toml"
  # The machine's network, as this case found it: `uninstall` removes it, so a run that did not put it back would
  # break `tests/container.rs` — and this machine's own egress network is not a container either case made.
  networks="$(docker network ls --format '{{.Name}}')"
  egress_before=0
  printf '%s\n' "$networks" | grep -qx 'egress' && egress_before=1

  # ---- 1. the tree: the unit goes, the data stays, the second run says nothing -------------------
  tree="$root/tree"
  install -d -m 0755 "$tree/etc/systemd/system" "$tree/data/secrets" ||
    fail "$name/tree" "could not create the install tree"
  install -m 0644 /dev/null "$tree/holosmith.service" ||
    fail "$name/tree/unit" "could not create the unit"
  install -m 0600 /dev/null "$tree/data/secrets/keys.toml" ||
    fail "$name/tree/keys" "could not create the key store"
  install -m 0600 /dev/null "$tree/data/secrets/vapid.toml" ||
    fail "$name/tree/vapid" "could not create the VAPID pair"
  status=0
  "$artifact" uninstall --root "$tree" --config "$WORK/config-uninstall.toml" \
    >"$root/first.out" 2>&1 || status=$?
  check "$name/first/exit" 0 "$status"
  grep -q '^uninstall: unit: removed: ' "$root/first.out" ||
    fail "$name/first/unit" "the unit was not reported removed: $(cat "$root/first.out")"
  assertions=$((assertions + 1))
  grep -q '^uninstall: data: kept: ' "$root/first.out" ||
    fail "$name/first/data" "the data was not reported kept: $(cat "$root/first.out")"
  assertions=$((assertions + 1))
  if [ "$egress_before" -eq 1 ]; then
    grep -q '^uninstall: network: removed: ' "$root/first.out" ||
      fail "$name/first/network" "the machine's egress network was there and is not reported removed: $(cat "$root/first.out")"
  else
    grep -q '^uninstall: network: nothing: ' "$root/first.out" ||
      fail "$name/first/network" "there was no egress network and the line claims one: $(cat "$root/first.out")"
  fi
  assertions=$((assertions + 1))
  [ ! -e "$tree/holosmith.service" ] ||
    fail "$name/first/file" "the unit file is still there"
  assertions=$((assertions + 1))
  # The data root and the secrets are what "keeps data" means, and the key store is the file a careless teardown
  # would take with it.
  [ -d "$tree/data" ] || fail "$name/first/root" "the data root was removed without --purge"
  assertions=$((assertions + 1))
  [ -f "$tree/data/secrets/keys.toml" ] ||
    fail "$name/first/keys" "the provider keys were removed without --purge"
  assertions=$((assertions + 1))
  [ -f "$tree/data/secrets/vapid.toml" ] ||
    fail "$name/first/vapid" "the VAPID pair was removed without --purge"
  assertions=$((assertions + 1))

  # The second run: nothing installed, nothing removed, and exit 0 — a teardown of nothing is not a failure.
  status=0
  "$artifact" uninstall --root "$tree" --config "$WORK/config-uninstall.toml" \
    >"$root/second.out" 2>&1 || status=$?
  check "$name/second/exit" 0 "$status"
  for what in unit proxies network; do
    grep -q "^uninstall: $what: nothing: " "$root/second.out" ||
      fail "$name/second/$what" "the second run does not say \`$what: nothing\`: $(cat "$root/second.out")"
    assertions=$((assertions + 1))
  done
  grep -q '^uninstall: data: kept: ' "$root/second.out" ||
    fail "$name/second/data" "the second run does not say \`data: kept\`: $(cat "$root/second.out")"
  assertions=$((assertions + 1))
  grep -q '^uninstall: resume: ' "$root/second.out" &&
    fail "$name/second/resume" "a successful run printed a resume command: $(cat "$root/second.out")"
  assertions=$((assertions + 1))

  # ---- 2. the two proxies: `rm --force` by the names the machine's install gave them -------------
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    printf 'SKIP %s/proxies: no %s image to stand in for the two proxies (tests/uninstall.rs covers the removal)\n' \
      "$name" "$image"
  else
    timeout 60 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
    docker run --detach --name hs_egress --entrypoint /bin/sleep "$image" 120 >/dev/null ||
      fail "$name/proxies/egress" "could not start a stand-in for hs_egress"
    docker run --detach --name hs_key --entrypoint /bin/sleep "$image" 120 >/dev/null ||
      fail "$name/proxies/key" "could not start a stand-in for hs_key"
    install -d -m 0755 "$tree/data" || fail "$name/proxies/tree" "could not recreate the data root"
    status=0
    "$artifact" uninstall --root "$tree" --config "$WORK/config-uninstall.toml" \
      >"$root/proxies.out" 2>&1 || status=$?
    check "$name/proxies/exit" 0 "$status"
    grep -q '^uninstall: proxies: removed: hs_egress and hs_key' "$root/proxies.out" ||
      fail "$name/proxies/line" "both proxies were running and the line does not say so: $(cat "$root/proxies.out")"
    assertions=$((assertions + 1))
    for proxy in hs_egress hs_key; do
      if docker inspect "$proxy" >/dev/null 2>&1; then
        fail "$name/proxies/gone" "$proxy outlived the teardown"
      fi
      assertions=$((assertions + 1))
    done
    timeout 60 docker rm -f hs_egress hs_key >/dev/null 2>&1 || true
  fi
  # ---- 3. --purge: refused over a live row, and it removes nothing --------------------------------
  tree="$root/purge"
  install -d -m 0755 "$tree/etc/systemd/system" ||
    fail "$name/purge/tree" "could not create the purge tree"
  install -m 0644 /dev/null "$tree/holosmith.service" ||
    fail "$name/purge/unit" "could not create the purge unit"
  if [ -n "$serve_pid" ] && kill -0 "$serve_pid" 2>/dev/null; then
    stop_serve
  fi
  control="$(free_port)" || fail "$name/port" "could not ask the kernel for a port"
  preview="$(free_port)" || fail "$name/port" "could not ask the kernel for a second port"
  # A boot of its own over the tree's data root: `--data-dir` is the override, and the tree's root is what the
  # `--root` uninstall will purge, so the row and the tree are the same install.
  start_serve "$WORK/config-uninstall.toml" "$tree/data"
  fetch_body "$name-create" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-uninstall-1" ||
    fail "$name/create" "POST /api/sessions did not answer"
  expect_status "$name/create" "$(answer "$name-create")" 202
  id="$(sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p' "$WORK/responses/$name-create.body" | sed -n '1p')"
  [ -n "$id" ] || fail "$name/create" "the create names no id: $(cat "$WORK/responses/$name-create.body")"
  wait_for_idle_id "$name-idle" "$id"

  status=0
  "$artifact" uninstall --root "$tree" --purge --config "$WORK/config-uninstall.toml" \
    >"$root/refused.out" 2>&1 || status=$?
  check "$name/refused/exit" 1 "$status"
  line="$(grep 'live' "$root/refused.out" || true)"
  [ -n "$line" ] || fail "$name/refused/sentence" "the refusal does not say what is live: $(cat "$root/refused.out")"
  case "$line" in
    *--force*) assertions=$((assertions + 1)) ;;
    *) fail "$name/refused/force" "the refusal names no override: $line" ;;
  esac
  # Nothing was removed: the refusal is before the first child, which is the whole reason it is asked first.
  [ -f "$tree/holosmith.service" ] ||
    fail "$name/refused/unit" "a refused purge removed the unit"
  assertions=$((assertions + 1))
  [ -d "$tree/data" ] || fail "$name/refused/root" "a refused purge removed the data root"
  assertions=$((assertions + 1))

  # With `--force`, the data root goes and the session that was live is named in the line.
  status=0
  "$artifact" uninstall --root "$tree" --purge --force --config "$WORK/config-uninstall.toml" \
    >"$root/forced.out" 2>&1 || status=$?
  check "$name/forced/exit" 0 "$status"
  grep -q '^uninstall: data: removed: ' "$root/forced.out" ||
    fail "$name/forced/data" "the purge did not report the data removed: $(cat "$root/forced.out")"
  assertions=$((assertions + 1))
  [ ! -e "$tree/data" ] || fail "$name/forced/root" "the data root outlived --purge"
  assertions=$((assertions + 1))
  stop_serve
  start_serve
  # The machine's network, back as this case found it — and **last**, because every `uninstall` above removed it
  # again (it is the machine's, not the tree's) and `tests/container.rs`'s egress test needs it. A case that left
  # the machine worse than it found it is the orphan a later change owns.
  if [ "$egress_before" -eq 1 ]; then
    docker network create egress >/dev/null 2>&1 ||
      fail "$name/restore" "could not put the machine's egress network back"
  fi
  rm -rf "$root"
}

# start_serve [config] [data dir]: boot the artifact and read the values the cases need from its
# banner. `case_token_budget` boots a second time with a config of its own, so both files are
# parameters, and so is the banner it reads: a case that ran against the first boot must not read the
# second's.
start_serve() {
  local config="${1:-$WORK/config.toml}" data="${2:-$WORK/data}" listen="${3:-0}" tag=serve
  [ "$data" = "$WORK/data" ] || tag="serve-$(basename "$data")"
  serve_err="$WORK/$tag.err"
  # --port 0 asks the kernel for the port, and it is also what makes serve re-derive
  # control_origin from the bound address (ADR-0007): the passing case can then send exactly
  # the origin the guard compares. --data-dir keeps the run out of /data.
  # --admin-dir keeps the drain socket out of /data/run, for the reason --data-dir keeps the database out of
  # /data: this sandbox lets an unprivileged user write the machine's paths, and a boot here must not
  # bind a real machine's admin socket. Every boot in this run shares the one directory under $WORK,
  # which is safe because only one serve is up at a time and a stale socket is replaced on bind.
  "$artifact" serve --config "$config" --port "$listen" --data-dir "$data" --admin-dir "$WORK/data/run" \
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
  # The preview listener's port too: the config asks for `127.0.0.1:0`, so this is the only place it can
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

# start_peer <config> <data> <listen>: a **second** machine on this box, with its own admin directory and its own
# `peer_*` globals.
#
# `start_serve` owns `serve_pid`, `port`, `control_origin`, `preview_port` and one admin socket under `$WORK`, and
# its comment says why that is safe: "only one serve is up at a time". A case that runs a hub *and* the server it
# forwards to breaks that, so the second machine gets its own function rather than a flag — and its own admin
# directory, because two processes must not probe one socket. Everything else is `start_serve`'s shape, including
# reading the bound ports from the banner rather than assuming them.
start_peer() {
  local config="$1" data="$2" listen="$3"
  peer_err="$WORK/serve-peer.err"
  "$artifact" serve --config "$config" --port "$listen" --data-dir "$data" \
    --admin-dir "$WORK/run/peer/holosmith" \
    >"$WORK/serve-peer.out" 2>"$peer_err" &
  peer_pid=$!
  for _ in $(seq 1 200); do
    grep -q '^control_listen ' "$peer_err" 2>/dev/null && break
    kill -0 "$peer_pid" 2>/dev/null || break
    sleep 0.05
  done
  peer_control_listen="$(sed -n 's/^control_listen //p' "$peer_err" | sed -n '1p')"
  [ -n "$peer_control_listen" ] ||
    fail "peer/banner" "the peer printed no control_listen line: $(cat "$peer_err")"
  peer_port="${peer_control_listen##*:}"
  peer_control_origin="$(sed -n 's/^control_origin //p' "$peer_err" | sed -n '1p')"
  [ -n "$peer_control_origin" ] ||
    fail "peer/banner" "the peer printed no control_origin line: $(cat "$peer_err")"
  peer_preview_listen="$(sed -n 's/^preview_listen //p' "$peer_err" | sed -n '1p')"
  case "$peer_preview_listen" in
    127.0.0.1:[0-9]*) peer_preview_port="${peer_preview_listen##*:}" ;;
    *)
      fail "peer/banner" "the peer's preview listener is not bound, and this run's cases need it: $peer_preview_listen"
      ;;
  esac
  printf 'ok peer/banner: listening on %s, control origin %s, preview %s\n' \
    "$peer_control_listen" "$peer_control_origin" "$peer_preview_listen"
}

# stop_peer: the second machine's `SIGTERM`, for a case that stops its peer itself rather than watching it die.
#
# No exit-code assertion here: the one case that runs a peer kills it, and *its* arm asserts what that ending is
# (`case_forward`'s 137). A check here would be a `check` nothing can reach, which is the shape this gate's own
# review rule calls a test that cannot fail.
stop_peer() {
  [ -n "${peer_pid:-}" ] || return 0
  kill -TERM "$peer_pid" 2>/dev/null || true
  wait "$peer_pid" 2>/dev/null || true
  peer_pid=""
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
  # *file*, so `--backend` is not what makes this boot work (the acceptance is both spellings;
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
    # ran, which is the one shape this gate may not have; nothing is swallowed, the lines are
    # printed below in order whatever they are.
    case_out="$WORK/case.out"
    "$case_fn" >"$case_out"
    cat "$case_out"
    if grep -q '^SKIP' "$case_out"; then
      skipped=$((skipped + 1))
    else
      # One line per case that measured something, so a green run names what it proved.
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
# The embedded-app assertions in `web.rs` carry a `HOLOSMITH_REQUIRE_WEB=1` arm — a placeholder build is legal
# (ADR-0007), so the tests cannot assert a real app unless a build says one exists. Setting it for this one phase,
# when a build is present, is what makes those assertions bite in the gate rather than pass vacuously (`case_pwa` is
# the measurement that matters, but a test that cannot fail is worse than no test — the gate rule).
#
# It is **scoped to this invocation** rather than exported: `scripts/check-embed.sh`'s second case builds with
# `web/dist` removed and *requires* the placeholder, so an exported variable would fail the `embed` phase. The gate
# found that, which is the reason this comment exists.
if [ -f web/dist/index.html ]; then
  HOLOSMITH_REQUIRE_WEB=1 run_phase test cargo test
else
  run_phase test cargo test
fi
# the expand/contract rule in one named command. The destructive side is refused where a migration is
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
