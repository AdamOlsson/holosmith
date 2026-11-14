# A run's children carry its name, and the next run cleans up the dead ones

A gate run is a process tree that is supposed to be gone when the run ends. `verify.sh`'s `cleanup` is an `EXIT`
trap, so it is gone when the run *fails* and when it *finishes* — and it is not gone when the run is killed, because
SIGKILL runs no trap. That is not an edge case on this machine: a worker's turn ends, a tool call times out, the
parent reaps the shell. The children do not die with it. They are reparented to `systemd --user` and stay.

Measured before deciding, on the development machine: four `node …/push-receiver.mjs` from two different checkouts
of this repository, all with `ppid 2458` = `systemd --user`, the oldest ~3 hours old, none of them attached to
anything alive; one `holosmith serve` from four hours earlier in the same state; 284 `target/verify-*` directories
across the repository's worktrees, 262 MB in one of them. The directories explain themselves: every case builds
`root="$repo_root/target/verify-<case>-$$"` and deletes it at its **start**, so a later run with a different pid
never touches an earlier one's — the delete is dead code across runs, and the count only ever grows.

## The decision

Ownership is inherited, not registered.

- **The marker.** `verify.sh` claims `target/verify-run-<pid>/owner` (`<pid> <starttime>`) before anything of the run
  exists, and exports the directory as `HOLOSMITH_GATE_RUN`. Every child a case spawns inherits it — the artifact, a
  second machine, a node receiver, a python stand-in, a `docker` CLI — with no spawn site doing anything. That is the
  property that matters: a hygiene mechanism each new spawn site has to remember is a mechanism that is wrong about
  the next case someone adds.
- **The cleanup.** `scripts/gate-cleanup.sh` kills only what carries a **dead** run's marker, and removes
  `target/verify-<name>-<pid>` whose trailing pid is gone. It runs at the start of `verify.sh`, under one
  repository-wide `flock`, and it warns rather than failing the gate.
- **Match exactly.** `environ` is NUL-separated and read as NUL-separated records, so the marker test is whole-element
  equality: `verify-run-12` cannot match `verify-run-123`. An argv *mention* of a dead run's path is reported, never
  signalled.
- **A pid is evidence in one direction.** A live pid spares both a stale record and a stale directory, even when the
  number now belongs to an unrelated process. A run whose pid was reused keeps its scratch forever; that is the
  accepted cost of never deleting a directory somebody is working in, and both directions are written down in the
  script so they are not "fixed" later.
- **`--legacy`** exists for what predates the marker, is printed before it acts, and requires a terminal or `--yes`.
  It kills three shapes only: an argv naming this repository's `target/verify-<name>-<dead pid>`; a `holosmith serve`
  booted from a temporary config with a worktree cwd; a push receiver in `/tmp` with a worktree cwd. Each is further
  restricted to executables this gate itself starts.

## Rejected, and why

- **A pidfile per spawn site.** `start_serve`, `start_peer`, `case_push`'s receiver and four more would each append
  to `target/verify-run-$$/pids`. It works for the spawn sites that exist and silently fails for the next one, which
  is exactly the failure being fixed. It also has no answer for the directories, which need no marker at all.
- **`setsid` plus a negative-pgid kill.** The run's process group belongs to whatever started it — an agent harness,
  a shell, a service manager — and a group kill would reach processes the gate never made. Per-child `setsid` fixes
  that and then depends on a pidfile again to find the children.
- **`pkill -f` on patterns.** The pattern that catches `holosmith serve` also catches the operator's own server, and
  on a machine with several worktrees it catches another run's live one. The marker is the only discriminator that
  survives both.
- **A lock per worktree.** Two runs of one worktree are the unrecoverable case: the second run can reuse the first's
  pid, so its record path is byte-identical to the stale record being cleaned, and a pass without a shared lock would
  kill a live run's children. Worktrees share one `.git` and one lock file because they share the machine and that
  pid space. A run in another worktree has a different record path, but it cleans this root too — which is the second
  reason the lock is repository-wide.
- **Containers, networks and sessions here.** Out of scope on purpose. A leaked serve's containers are the next
  boot's reconcile, and `s_<id>` is a real session's name; a tool that deletes by prefix would eventually delete a
  session someone is using.

## Consequences

- Every `verify.sh` run leaves one small directory — `target/verify-run-<pid>/` — for the next cleanup to report,
  take any survivor that still carries it, and remove. A green run's record is left too, deliberately: the trap cannot
  prove it left no survivor (the `extra_pid` slot is shared, and a case can forget its own child — `case_cross_machine`
  did, on the green path, which is how this was found), and a record removed by the run it names is a survivor with no
  address.
- `case_gate_cleanup` is a gate case of its own: the marker, the prefix that must not match, the lock, an
  unremovable directory, a symlinked root, a path outside every root, and the `--legacy` refusals. The two things it
  cannot test are written down instead of implied — the pid-reuse interleaving (a pid cannot be forced to wrap, so
  the mutex is what is asserted) and a descendant that scrubbed its environment (invisible by construction).
- The gate is Linux-shaped here: the marker is read from `/proc/<pid>/environ`, and a host without `/proc` is told so
  in one sentence and cleaned by the directory rule only. That is consistent with a gate that already requires a musl
  artifact, `/dev/tcp` and a Docker daemon.
- The cleanup never fails the gate. A machine-hygiene problem warns; a leftover listener on a pinned port already fails
  in the case that wanted it, and a red gate must mean the tree.
- `A worker's gate is one run` (AGENTS.md) is unchanged: `gate-quick.sh` claims no record and cleans nothing, because
  a worker's leak is the parent's next `verify.sh` to clear, and `case_gate_quick` asserts that script's output line
  by line.
