# A start is `rm -f` then `run`, serialised per session, and `resume` gets a body

This is spec item 6's first three lines: "`--rm` means a stopped container never leaves its name behind.
Before every start, run `docker rm -f s_<id>` and ignore not-found. Session data lives in host directories,
so removing the container loses nothing." Plus one sentence that is not about Docker at all: "Two starts for
the same session cannot race: a per-session lock serialises them."

## `--rm` is not enough, and the measurement says why

`--rm` removes a container when it **exits**, and it does so a moment *after* `docker stop` returns: the
removal is the daemon's own asynchronous work, not part of the stop's answer. The first draft of
`tests/container.rs`'s `a_second_start_never_fails_on_a_leftover_name` asserted the name was free the
instant `stop` answered and failed on it — measured, not guessed.

So the spec's `rm -f` is not belt-and-braces over a flag that already works. It is what covers:

- the window above, where the container has exited but the daemon has not removed it yet;
- a container this process has lost track of — a `kill -9`, a crash, a session row removed while its
  container lived;
- a container an operator started by hand under the same name. (reconciles orphans at startup; this
  makes the *next start* boring regardless.)

`rm -f` before `run` is one call inside the container step, so every path that starts a container — a
create, a resume, a retry — gets it, and one function owns the rule. A container that is not there is an
ordinary answer, not a log line (ADR-0023's rule, applied to a resource that a policy may have removed).

## The start lock is the backend's, and it is `tokio`'s

A start is two daemon calls (`rm -f`, `run`) that must not interleave with another start of the same
session: interleaved, the second `run` fails with "the container name is already in use", which is a failure
neither caller can interpret — it is the *desired* behaviour of the other one being read as an error.

The lock lives in `DockerBackend`, not in the endpoint, because it protects the backend's resource: a lock
the handler holds would protect only the handler, and `resume` has a different caller from `create` (the
provisioner, not the request). It is a `tokio::sync::Mutex` per session id because it is held across an
`.await`; the `Arc<Mutex<HashMap<…>>>` behind it is `std`'s and is never held across one (ADR-0012's shape,
and for the same reason). The map's entry is dropped when it is the last holder's, so it is swept rather
than grown by one entry per session this process ever served.

**Mutation-proved:** delete the lock and `two_starts_of_one_session_do_not_race` fails on all three runs
tried, with seven of eight starts reporting `the container name "/s_…" is already in use`. Restored, it
passes.

## `stop` and `resume` stop refusing

`SessionBackend::resume` has been uncalled since ADR-0022 ("no stop endpoint; resume is the only way back,
and `SessionBackend::resume` stays uncalled") — the endpoint re-provisions instead. This story gives it a
body, because the acceptance's third line ("a test that stops, removes and resumes") needs one:

- `stop` is `docker stop --time 10`, the spec's own line: the ten seconds are what lets a process flush what
  it is writing, and `--rm` is what removes the container afterwards, so a stop needs no removal of its own.
  A container that is not there answers `Ok`.
- `resume` takes the start lock, re-creates the session's network if it is gone, and starts the container. It
  touches **no** worktree: the worktree is the provisioner's step and its `ensure` is create-or-adopt
  (ADR-0029), so a resumed session's commits are still on its branch.

The expected-stop **flag** and the exit classification are deliberately not here — those are that story's, and
`stop` is the call they will annotate.

## What is asserted, and where

`tests/container.rs` gains three cases: a start after a stop and a start after a *planted* leftover
container both succeed; a commit made inside the container survives a stop, a removal and a resume (the
branch's head and the file are both still there, and the new container finds them); and eight concurrent
starts all succeed and leave exactly one container. `scripts/verify.sh` gains `case_docker_resume`, because
the artifact can show the other half: `POST /resume` on a session that ended at the agent step answers 202,
runs the network and container steps again, and leaves nothing behind.
