#!/usr/bin/env bash
# Clean up after gate runs that were killed, and claim the record that names the running one. Why a run's children
# need a name at all, every shape it refuses, and the reasons behind each rule: **ADR-0089**. The rules, one line
# each: kill only a *dead* run's marked children; remove only `target/verify-<name>-<pid>` under a resolved root whose
# trailing pid is gone; never touch a container, a network, a session row, or a process that merely mentions a dead
# run's path; and a live pid spares both a record and a directory — that last one is deliberate, do not "fix" it.
#
# Usage: scripts/gate-cleanup.sh [--dry-run] [--quiet] [--claim [--owner PID]] [--legacy [--yes]] [--worktree DIR]...
#
# Exit 0 nothing left behind, 1 a process we signalled survived or a directory could not be removed, 2 misuse. The two
# streams are separate facts, because a claim and a cleanup can disagree: **stdout carries the record path and nothing
# else, also when the exit is 1**, while every warning and report line goes to stderr. A caller captures both:
#
#   run_root="$(scripts/gate-cleanup.sh --claim --owner "$$" 2>"$err")" || claim_rc=$?
#
# `--owner` is required of a caller that reads the path with `$(...)`: inside a command substitution `$PPID` is the
# substitution's own shell, which dies at once, and a record naming it reads as a dead run to the next cleanup.
# `scripts/verify.sh` is the canonical caller and its comment says why `if cmd; then` is wrong here.

set -euo pipefail

# Prints this script's header comment, whatever its line count.
usage() {
  sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

# misuse <sentence>: exit 2, the code that cannot be read as a pass.
misuse() {
  printf 'gate-cleanup: %s\n' "$1" >&2
  printf 'usage: scripts/gate-cleanup.sh [--dry-run] [--quiet] [--claim [--owner PID]] [--legacy [--yes]] [--worktree DIR]...\n' >&2
  exit 2
}

# say <sentence>: every report line, always on stderr. `--claim`'s stdout is the record path and nothing else.
say() {
  printf 'gate-cleanup: %s\n' "$*" >&2
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

dry_run=0
quiet=0
legacy=0
assume_yes=0
claim=0
claim_owner=""
explicit_roots=()
roots=()
grace="${GATE_CLEANUP_GRACE:-5}"

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --quiet) quiet=1 ;;
    --claim) claim=1 ;;
    --legacy) legacy=1 ;;
    --yes) assume_yes=1 ;;
    --owner)
      shift
      [ $# -gt 0 ] || misuse "--owner needs a pid"
      claim_owner="$1"
      ;;
    --worktree)
      shift
      [ $# -gt 0 ] || misuse "--worktree needs a directory"
      explicit_roots+=("$1")
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *) misuse "unknown argument: $1" ;;
  esac
  shift
done

# ---- which roots, resolved -----------------------------------------------------
# Every checkout of this repository when the caller names none: they share one `.git`, one machine and one port
# space, so an orphan in a sibling worktree is this run's problem too. `--worktree` exists for the gate's own case,
# which must be able to clean a scratch tree and touch nothing real.
if [ "${#explicit_roots[@]}" -eq 0 ]; then
  roots=("$repo_root")
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    roots+=("$line")
  done < <(git worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p')
else
  roots=("${explicit_roots[@]}")
fi

resolved=()
for root in "${roots[@]}"; do
  [ -d "$root" ] || misuse "not a directory: $root"
  real="$(realpath -e -- "$root" 2>/dev/null)" || misuse "cannot resolve --worktree $root"
  [ "$real" = "/" ] && misuse "refusing to clean /"
  duplicate=0
  for have in "${resolved[@]}"; do
    [ "$have" = "$real" ] && duplicate=1
  done
  [ "$duplicate" -eq 0 ] && resolved+=("$real")
done
[ "${#resolved[@]}" -gt 0 ] || misuse "no roots to clean"

# ---- the lock ------------------------------------------------------------------
# One lock per repository, held for the cleanup and the claim and never for a gate run. It closes the one race that is
# unrecoverable: two runs of *one* worktree where the second reuses the first's pid, so its record path is
# byte-identical to the stale one being cleaned, and the pass would kill a live run's children by marker. A run in
# another worktree has a different record path, but it cleans this root too, which is why the lock is per repository
# rather than per worktree.
if [ "${#explicit_roots[@]}" -gt 0 ]; then
  lock_root="${resolved[0]}"
else
  common="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
  case "$common" in
    */.git) lock_root="${common%/.git}" ;;
    *) lock_root="$repo_root" ;;
  esac
fi
lock="$lock_root/target/gate-cleanup.lock"
mkdir -p "$lock_root/target" || misuse "could not create $lock_root/target"
if command -v flock >/dev/null 2>&1; then
  # `exec` with a redirect aborts the script under `set -e` with a bare bash error, which is not a sentence a
  # reader can act on: open it inside an `if`, where a failure is ours to report.
  if ! exec 9>>"$lock"; then
    misuse "cannot write $lock — nothing was done"
  fi
  flock -w 30 9 || misuse "another gate-cleanup held $lock for 30s; nothing was done"
else
  say "flock not found — cleaning up without the lock, so two runs may interleave"
fi

# ---- /proc primitives ----------------------------------------------------------
# Without /proc nothing here can tell a live pid from a dead one, and every scratch directory would look removable.
# That is the one case where doing nothing is right, and it is said out loud instead of guessed at.
have_proc=0
[ -r /proc/self/stat ] && have_proc=1

# proc_field <pid> <n>: field N of /proc/<pid>/stat counted from `state` — 2 is the parent, 20 the starttime in
# jiffies, which is the one value that tells a live run's pid from the same number reassigned to something else. The
# comm is free-form (spaces, '(' and ')'), so everything through the **last** ") " is stripped first: the fields that
# follow contain no ") ", which makes that strip the comm's terminator and never a ')' inside the name.
proc_field() {
  local stat
  local -a field
  stat="$(cat "/proc/$1/stat" 2>/dev/null)" || return 1
  read -r -a field <<<"${stat##*) }"
  [ "${#field[@]}" -ge "$2" ] || return 1
  printf '%s' "${field[$(($2 - 1))]}"
}

# read_argv <pid>: fills ARGV. A function cannot return an array, and one global is clearer than four parsers.
ARGV=()
read_argv() {
  ARGV=()
  local arg
  # `[ -r ]` before the loop, not `2>/dev/null` on it: the redirect's own failure is printed by the shell, not by
  # the command, so a stderr redirection would silence the read and not the `Permission denied` it is hiding.
  [ -r "/proc/$1/cmdline" ] || return 0
  while IFS= read -r -d '' arg; do
    ARGV+=("$arg")
  done <"/proc/$1/cmdline" 2>/dev/null || true
}

# proc_label <pid>: one line for a report, never a reason to do anything.
proc_label() {
  local line=""
  if [ -r "/proc/$1/cmdline" ]; then
    line="$(tr '\0' ' ' <"/proc/$1/cmdline" 2>/dev/null | cut -c1-160 2>/dev/null || true)"
  fi
  printf '%s' "${line:-?}"
}

# carried_record <pid>: 0 and `CARRY` set to the record when the process carries one of the stale records, matched
# as a **whole** `HOLOSMITH_GATE_RUN=<record>` element of the NUL-separated environ — so `verify-run-12` can never
# match `verify-run-123`, and no prefix rule is involved. One read of `environ` per process, whatever the number of
# stale records: a machine that has been killed ten times must not cost ten scans of every process before the first
# phase. An unreadable environ is not evidence of anything.
CARRY=""
carried_record() {
  local pid="$1" entry value record
  CARRY=""
  [ -r "/proc/$pid/environ" ] || return 1
  while IFS= read -r -d '' entry; do
    case "$entry" in
      HOLOSMITH_GATE_RUN=*) value="${entry#HOLOSMITH_GATE_RUN=}" ;;
      *) continue ;;
    esac
    for record in "${stale_records[@]}"; do
      [ "$value" = "$record" ] || continue
      CARRY="$record"
      return 0
    done
  done <"/proc/$pid/environ" 2>/dev/null || true
  return 1
}

# mentioned_record <pid>: 0 when argv names a stale record or something under it, for the same one-read reason and
# with the same '/' boundary, which is what keeps `verify-run-12` and `verify-run-123` different names here too.
MENTION=""
mentioned_record() {
  local pid="$1" arg record
  MENTION=""
  [ -r "/proc/$pid/cmdline" ] || return 1
  while IFS= read -r -d '' arg; do
    [ -n "$arg" ] || continue
    for record in "${stale_records[@]}"; do
      [ "$arg" = "$record" ] && {
        MENTION="$record"
        return 0
      }
      case "$arg" in
        "$record"/*)
          MENTION="$record"
          return 0
          ;;
      esac
    done
  done <"/proc/$pid/cmdline" 2>/dev/null || true
  return 1
}

# record_state <record dir>: live, stale or unknown.
#   live     the owner pid is alive *and* its starttime is the one recorded
#   stale    the owner pid is gone, or the number now belongs to another process
#   unknown  no readable owner file, or no /proc to ask — spared, because a record we cannot read is not a licence
#            to kill whatever carries its path
record_state() {
  local record="$1" pid="" start="" now
  [ -r "$record/owner" ] || {
    printf 'unknown'
    return 0
  }
  read -r pid start <"$record/owner" 2>/dev/null || {
    printf 'unknown'
    return 0
  }
  case "$pid" in
    ''|*[!0-9]*) printf 'unknown'; return 0 ;;
  esac
  [ -d "/proc/$pid" ] || {
    printf 'stale'
    return 0
  }
  now="$(proc_field "$pid" 20 2>/dev/null || true)"
  if [ -n "$now" ] && [ "$now" = "$start" ]; then
    printf 'live'
  else
    printf 'stale'
  fi
}

# This script's own ancestry: never a target, whatever it mentions. `$$` alone is not enough — the shell that
# called us, the agent above it, and a shell running `grep` beside it are all processes whose argv may name a
# record path.
self_chain="|$$|$BASHPID|"
walk="$BASHPID"
hops=0
while [ "$have_proc" -eq 1 ] && [ "$hops" -lt 64 ]; do
  ppid="$(proc_field "$walk" 2 2>/dev/null || true)"
  case "$ppid" in
    ''|*[!0-9]*) break ;;
  esac
  [ "$ppid" -gt 1 ] || break
  self_chain="$self_chain$ppid|"
  walk="$ppid"
  hops=$((hops + 1))
done

is_ancestor() {
  case "$self_chain" in
    *"|$1|"*) return 0 ;;
  esac
  return 1
}

# The gate starts these and nothing else; a legacy kill is restricted to them so an editor, a pager or a grep that
# merely has a record path in its argv cannot be reached even with --yes.
allowed_executable() {
  local name="$1"
  [ "$name" = "holosmith" ] && return 0
  [ "$name" = "node" ] && return 0
  [ "$name" = "python3" ] && return 0
  [ "$name" = "python" ] && return 0
  [ "$name" = "sleep" ] && return 0
  [ "$name" = "docker" ] && return 0
  [ "$name" = "sh" ] && return 0
  [ "$name" = "bash" ] && return 0
  return 1
}

# ---- legacy shapes -------------------------------------------------------------
# Opt-in, printed before anything is signalled, and confirmed. Three shapes, all of them things only this gate
# makes: (a) an argv naming this repository's `target/verify-<name>-<dead pid>`, which covers every case that puts
# its scratch under `target/`; (b) a `holosmith serve` booted from a temporary config with a worktree cwd; (c) a
# push receiver in /tmp with a worktree cwd. (b) is the shape most likely to be somebody's own demo server, so it
# is reported under its own heading.
legacy_shape() {
  local pid="$1" exe arg owner_pid cwd under_root root has_serve i rest previous component
  read_argv "$pid"
  [ "${#ARGV[@]}" -gt 0 ] || return 0
  exe="${ARGV[0]##*/}"
  allowed_executable "$exe" || return 0

  for arg in "${ARGV[@]}"; do
    # Only a path under a resolved root is this repository's business; the same shape in another tree is somebody
    # else's and is not ours to kill (the header's promise, and what lets --worktree isolate the gate's own case).
    for root in "${resolved[@]}"; do
      case "$arg" in
        "$root"/target/*) ;;
        *) continue ;;
      esac
      # The piece after `target/` is the component that names the run, and the pid is its last field: the argument
      # itself may go on (`.../verify-cross-123/tls/server.pem`), so a suffix of the whole string would read `pem`.
      rest="$arg"
      previous=""
      while [ -n "$rest" ]; do
        component="${rest%%/*}"
        case "$rest" in
          */*) rest="${rest#*/}" ;;
          *) rest="" ;;
        esac
        if [ "$previous" = "target" ]; then
          case "$component" in
            verify-*-[0-9]*)
              owner_pid="${component##*-}"
              case "$owner_pid" in
                ''|*[!0-9]*) ;;
                *)
                  if [ ! -d "/proc/$owner_pid" ]; then
                    printf 'a'
                    return 0
                  fi
                  ;;
              esac
              ;;
          esac
        fi
        previous="$component"
      done
    done
  done

  # (b) and (c) only mean something when the process was started in a checkout of this repository.
  cwd="$(readlink -f "/proc/$pid/cwd" 2>/dev/null || true)"
  under_root=0
  for root in "${resolved[@]}"; do
    [ -n "$cwd" ] && [ "$cwd" = "$root" ] && under_root=1
  done
  [ "$under_root" -eq 1 ] || return 0

  case "$exe" in
    holosmith)
      has_serve=0
      for arg in "${ARGV[@]}"; do
        [ "$arg" = "serve" ] && has_serve=1
      done
      [ "$has_serve" -eq 1 ] || return 0
      i=0
      while [ "$i" -lt "${#ARGV[@]}" ]; do
        arg="${ARGV[$i]}"
        case "$arg" in
          --config=*|--data-dir=*)
            case "$arg" in
              *"=/tmp/tmp."*) printf 'b'; return 0 ;;
            esac
            ;;
          --config|--data-dir)
            i=$((i + 1))
            arg="${ARGV[$i]:-}"
            case "$arg" in
              /tmp/tmp.*) printf 'b'; return 0 ;;
            esac
            ;;
        esac
        i=$((i + 1))
      done
      ;;
    node)
      for arg in "${ARGV[@]}"; do
        case "$arg" in
          /tmp/*/push-receiver.mjs) printf 'c'; return 0 ;;
        esac
      done
      ;;
  esac
  return 0
}

# ---- collect: records first, so a live run is known before anything is signalled ----
stale_records=()
live_records=()
targets=()
target_records=()
target_labels=()
declare -A seen_pid=()
mentions=()
legacy_pids=()
legacy_shapes=()
legacy_serves=()

if [ "$have_proc" -eq 0 ]; then
  say "no readable /proc on this host: no pid can be classified, so nothing is cleaned"
else
  for root in "${resolved[@]}"; do
    [ -d "$root/target" ] || continue
    for dir in "$root"/target/verify-run-*; do
      [ -d "$dir" ] || continue
      case "$(record_state "$dir")" in
        live) live_records+=("$dir") ;;
        stale) stale_records+=("$dir") ;;
        *)
          [ "$quiet" -eq 1 ] || say "spared $dir: its owner record is unreadable, so it is not evidence"
          ;;
      esac
    done
  done

  # One pass over /proc for every stale record at once: a process is read twice (environ, cmdline) and compared
  # against the records, rather than each record re-reading every process. A pid that carries a marker is a target
  # and is not also reported as a mention — it is about to be killed, and a line saying "not killed" about it would be
  # a lie. A process that names a dead run's record but does not carry it *is* reported and never signalled, which is
  # the rule that keeps a `grep` on the path, or an editor with the owner file open, out of the kill list.
  if [ "${#stale_records[@]}" -gt 0 ]; then
    for dir in /proc/[0-9]*; do
      [ -d "$dir" ] || continue
      pid="${dir#/proc/}"
      [ "$pid" = "$$" ] && continue
      [ "$pid" = "$BASHPID" ] && continue
      is_ancestor "$pid" && continue
      if carried_record "$pid"; then
        [ "${seen_pid[$pid]:-}" = "1" ] && continue
        seen_pid[$pid]=1
        targets+=("$pid")
        target_records+=("$CARRY")
        target_labels+=("marked by $(basename "$CARRY")")
      elif mentioned_record "$pid"; then
        mentions+=("$(proc_label "$pid")")
      fi
    done
  fi

  if [ "$legacy" -eq 1 ]; then
    for dir in /proc/[0-9]*; do
      [ -d "$dir" ] || continue
      pid="${dir#/proc/}"
      [ "$pid" = "$$" ] && continue
      [ "$pid" = "$BASHPID" ] && continue
      is_ancestor "$pid" && continue
      shape="$(legacy_shape "$pid")"
      [ -n "$shape" ] || continue
      legacy_pids+=("$pid")
      legacy_shapes+=("$shape")
      # `pid|cwd|argv` for (b): this is the shape an operator is most likely to recognise as their own, so the line
      # that asks them to look has to carry the pid and the directory it was started in, not only its arguments.
      if [ "$shape" = "b" ]; then
        legacy_serves+=("$pid|$(readlink -f "/proc/$pid/cwd" 2>/dev/null || printf '?')|$(proc_label "$pid")")
      fi
    done
  fi
fi

# ---- report, confirm, act -----------------------------------------------------------------------------------
for record in "${stale_records[@]}"; do
  [ "$quiet" -eq 1 ] || say "dead run $(basename "$record") in $(dirname "$(dirname "$record")")"
done
for record in "${live_records[@]}"; do
  [ "$quiet" -eq 1 ] || say "live run $(basename "$record") — spared"
done

for i in "${!targets[@]}"; do
  pid="${targets[$i]}"
  if [ "$dry_run" -eq 1 ]; then
    say "would kill $pid (${target_labels[$i]}): $(proc_label "$pid")"
    continue
  fi
  # Re-read the record immediately before the signal: the snapshot above is a snapshot, and the lock makes that
  # window small rather than absent.
  [ "$(record_state "${target_records[$i]}")" = "stale" ] || continue
  [ -d "/proc/$pid" ] || continue
  kill -TERM "$pid" 2>/dev/null || true
done

if [ "${#legacy_pids[@]}" -gt 0 ]; then
  if [ "${#legacy_serves[@]}" -gt 0 ]; then
    say "legacy (b) — a holosmith serve booted from a temporary config:"
    for entry in "${legacy_serves[@]}"; do
      say "  pid ${entry%%|*} cwd $(printf '%s' "${entry#*|}" | cut -d'|' -f1): $(printf '%s' "${entry#*|*|}")"
    done
    say "WARNING: this is the shape most likely to be your own demo server rather than a gate's. Read the argv above."
  fi
  for i in "${!legacy_pids[@]}"; do
    pid="${legacy_pids[$i]}"
    shape="${legacy_shapes[$i]}"
    case "$shape" in
      a) say "legacy (a) — a path under target/ named by a dead pid: $pid ($(proc_label "$pid"))" ;;
      c) say "legacy (c) — a push receiver in a temporary directory: $pid ($(proc_label "$pid"))" ;;
      b) [ "${#legacy_serves[@]}" -gt 0 ] || say "legacy (b) — a serve from a temporary config: $pid ($(proc_label "$pid"))" ;;
    esac
  done
  if [ "$dry_run" -eq 0 ]; then
    if [ "$assume_yes" -eq 0 ]; then
      [ -t 0 ] || misuse "--legacy kills processes no marker accounts for: run it with --yes, or from a terminal"
      printf 'gate-cleanup: type legacy to kill these: ' >&2
      reply=""
      read -r reply || true
      if [ "$reply" != "legacy" ]; then
        say "nothing killed: the confirmation did not match"
        legacy_pids=()
      fi
    fi
    for pid in "${legacy_pids[@]}"; do
      [ -d "/proc/$pid" ] || continue
      kill -TERM "$pid" 2>/dev/null || true
    done
  fi
fi

# The grace in tenths of a second, one poll for everything, then KILL. Best effort and per process: a child of a
# target that survives its parent is reached only if it carries the marker, which is the sentence in the header.
survivors=()
if [ "$dry_run" -eq 0 ] && { [ "${#targets[@]}" -gt 0 ] || [ "${#legacy_pids[@]}" -gt 0 ]; }; then
  ticks=0
  while [ "$ticks" -lt $((grace * 10)) ]; do
    left=0
    for pid in "${targets[@]}" "${legacy_pids[@]}"; do
      [ -d "/proc/$pid" ] && left=1
    done
    [ "$left" -eq 0 ] && break
    sleep 0.1
    ticks=$((ticks + 1))
  done
  for pid in "${targets[@]}" "${legacy_pids[@]}"; do
    [ -d "/proc/$pid" ] || continue
    kill -KILL "$pid" 2>/dev/null || true
    sleep 0.1
    [ -d "/proc/$pid" ] && survivors+=("$pid")
  done
fi

# ---- the directories ---------------------------------------------------------------------------------------
# Retroactive and marker-free: `verify-<name>-<pid>` under a resolved root's `target/`, with a **dead** trailing
# pid. This is the only `rm -rf` in the script, and its argument is always that one shape under a resolved root.
removed=0
failed=0
failed_paths=()
for root in "${resolved[@]}"; do
  [ -d "$root/target" ] || continue
  for dir in "$root"/target/verify-*; do
    [ -d "$dir" ] || continue
    base="${dir##*/}"
    case "$base" in
      verify-*-*) ;;
      *) continue ;;
    esac
    case "${base#verify-}" in
      *[!a-z0-9-]*) continue ;;
    esac
    dir_pid="${base##*-}"
    case "$dir_pid" in
      ''|*[!0-9]*) continue ;;
    esac
    # A live pid spares the directory even when its run is long dead — a fact about the pid, not a failure — so it is
    # reported rather than passed over in silence, and it never touches the exit code.
    if [ "$have_proc" -eq 1 ] && [ -d "/proc/$dir_pid" ]; then
      [ "$quiet" -eq 1 ] || say "kept $base: pid $dir_pid is alive"
      continue
    fi
    if [ "$dry_run" -eq 1 ]; then
      say "would remove $dir"
      continue
    fi
    if rm -rf -- "$dir" 2>/dev/null && [ ! -e "$dir" ]; then
      removed=$((removed + 1))
    else
      failed=$((failed + 1))
      failed_paths+=("$dir")
      say "could not remove $dir"
    fi
  done
done

# ---- claim -------------------------------------------------------------------------------------------------
if [ "$claim" -eq 1 ]; then
  owner="${claim_owner:-$PPID}"
  case "$owner" in
    ''|*[!0-9]*) misuse "--owner needs a pid, not '$owner'" ;;
  esac
  start="$(proc_field "$owner" 20 2>/dev/null || true)"
  record="${resolved[0]}/target/verify-run-$owner"
  if [ "$dry_run" -eq 0 ]; then
    mkdir -p "$record" || misuse "could not create $record"
    printf '%s %s\n' "$owner" "${start:-0}" >"$record/owner"
  fi
  printf '%s\n' "$record"
fi

# ---- the last lines ----------------------------------------------------------------------------------------
if [ "${#mentions[@]}" -gt 0 ]; then
  say "${#mentions[@]} process(es) mention a dead run's path but carry no marker — not killed (--legacy lists them):"
  for line in "${mentions[@]}"; do
    say "  $line"
  done
fi

if [ "${#survivors[@]}" -gt 0 ]; then
  say "${#survivors[@]} process(es) survived SIGKILL: ${survivors[*]}"
fi
if [ "$failed" -gt 0 ]; then
  # One command to run, once per cleanup: a directory this account may not delete is root's, and the operator's next
  # move is a `sudo` they should not have to assemble from the paths above.
  say "$failed director(ies) could not be removed; clear them with:"
  # shellcheck disable=SC2016 # deliberate: this is a command to read and run, not one this script runs.
  say "  sudo rm -rf $(printf '%q ' "${failed_paths[@]}")"
fi

# The summary is about the cleanup's own work. A removal that failed has its own line above and does not need to be
# repeated as "removed 0", which is what a machine with one root-owned directory would print at every boot.
if [ "${#stale_records[@]}" -gt 0 ] || [ "$removed" -gt 0 ]; then
  say "removed $removed director(ies), cleaned ${#stale_records[@]} dead run(s), ${#live_records[@]} live run(s) spared"
fi

if [ "${#survivors[@]}" -gt 0 ] || [ "$failed" -gt 0 ]; then
  exit 1
fi
exit 0
