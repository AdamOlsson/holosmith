# The disk soft cap is a watchdog over the row's own worktree, and it ends a turn with the ladder's second rung

HS-063 is spec item 14's second sentence: "`session_disk_soft_gb` (default 10): a watchdog runs `du` on each
live session's worktree every 5 minutes. Over the limit it emits an `error` event with code `disk_quota`,
aborts the turn and stops the session (resumable after cleanup)." The key has existed since HS-034 and nothing
read it. Six things here were open, and one of them — how a *periodic* policy ends a *turn* — is the part the
next story of this shape will copy.

## D1 — a module and a task of its own, `0` is the off switch, the tick is the spec's

`src/quota.rs`; `AppState::quota() -> Option<Quota>`, spawned in `commands/serve.rs` beside the sweeper and
aborted with it. Not a second pass in the sweeper: the sweeper's tick is five seconds and this one's is five
minutes, and a policy is not a period. `TICK` is a constant rather than a config key for ADR-0025's reason —
the spec's config list is exhaustive, and how often a process looks at a row is not a machine setting, whereas
*this* interval is the policy itself, which the spec names. `session_disk_soft_gb = 0` is off, exactly like
`idle_stop_min = 0` and `min_free_gb = 0`: `quota()` answers `None`, so no task is built, no `du` runs, and the
number never reaches the comparison (against `0` every worktree would be "over"). No new config key, no new
validation rule.

## D2 — `du -sk`, through the one seam a test can own a program with

The reader is `du -sk -- <worktree>`, synchronously, on `spawn_blocking`, with the database lock dropped
first — a `du` of a multi-gigabyte tree is hundreds of milliseconds, and the compactor's "hold the mutex
because the fold is synchronous" argument does not transfer to a subprocess. It goes through
`preflight::Tools`, which gains a `du` beside `git` and `df`: that struct is the build's one way to put a
program under a test's control, because this crate denies `unsafe` and cannot set `PATH` (HS-024, HS-062).

No `--one-file-system`: it is a GNU option, `du` here must be a POSIX utility, and the cap's point is that
everything under the worktree counts — `target/`, `node_modules/`, and a nested mount too. The arithmetic is
in `u128`: `du` answers KiB and the key is whole GiB, so `size_kib > limit_gb * 1024 * 1024`, which cannot wrap
for an absurd config value. A `du` that is missing, refuses, or prints something that is not a `du -sk`
answer measures **nothing**: no conclusion is drawn, the row is untouched and the next tick asks again —
"a check that cannot run is not a pass", which item 14's `.is` rule and HS-062's four arms already say.

## D3 — the candidates are the rows, and the predicate is a pure function

`watchable(&[Session])` filters the `list(None)` answer the reconciler and the compactor already read:
`running` and `idle`, and only rows whose worktree column is set. No new SQL, so no new statement to guard.
`starting` is deliberately **not** a candidate: a fresh row has no worktree, and a recovered one is being
re-provisioned, so a pass over it would race the provisioner for the row. It is also why one crossing is one
event — a row this policy stopped is no longer a candidate, so "an event per tick for ever" cannot happen.

## D4 — the turn ends through `Ladder::end_turn`, and the row stops; nothing here resumes

The `running` arm calls `end_turn(session, DISK_QUOTA)`: rung 1 clears the queue and asks the agent to abort,
rung 2 — the table's `abort_ladder` row, with *this* caller's reason — moves `running → stopped`, and the
container goes through `Stopper`, so `expected_stop` is written before the stop. That is the third caller of
the shape the turn timeout (HS-022) and the token budget (HS-060) use, and the reason word is the caller's
exactly as `turn_timeout` and `budget_exceeded` are.

**`climb` is not used.** It is the same two rungs *plus the way back*: a forced stop is resumed at once,
because a killed container is not a session the operator asked to keep stopped. A session over its disk cap is
the opposite — it must stay `stopped` until somebody has cleaned the worktree up, which is what "resumable
after cleanup" means. So `end_turn` returns `Ending::Asked` (rung 1 worked; the turn runner wrote
`running → idle` with `abort_completed`) or `Ending::Forced` (rung 2 wrote `stopped` with `disk_quota`), and:

- `Forced` is the end: the row is `stopped`, the container is stopped, and the event is written.
- `Asked` leaves an `idle` row, and the idle arm finishes the job: the table gains a `disk_quota` row
  (`idle → stopped`, the amendment `pi_exit` was in ADR-0038), the row moves first, and the container's stop is
  spawned — the sweeper's shape, because the row is what frees the slot.
- Anything else (a delete, a resume that won the race) is an `info` line: the candidate set moved under us,
  nothing is stopped, and the next tick re-measures.

The mission word is one constant, `quota::DISK_QUOTA`: the reason the row records, the table's trigger word and
the event's `code`. ADR-0008 says a `code` is effectively an API, so three spellings of one fact would be three
things a client learns.

## D5 — the event is a new kind, `error`, and the stop is the row's

Spec item 14 says "an `error` event with code `disk_quota`", and the build's kind list had no `error`: its
`turn.error` is about the *agent* failing at something, and this is the machine deciding the session must stop.
A new kind is additive — ADR-0018 makes an unknown kind dropped, not refused — so old clients ignore it and this
build's client renders it. Payload: `{code, message, size_kib, limit_gb}`. `code` is the row's reason word;
`size_kib` is what was measured and `limit_gb` the cap it crossed, because a client reading a durable log later
has no config to consult; `message` is the sentence a screen shows, composed once by the producer from both
numbers (a whole number of GB would hide the crossing — a 10.5 GB tree floored is "10 GB over a 10 GB cap").

It is written **once per crossing and only after the row is `stopped`** (a refused transition writes nothing),
which is the honest order: the log reports a fact, it does not predict one. The *stop* is `stopped`, never
`error` — the story's third acceptance bullet — and that is the row, not the frame: a client that wants to know
whether the work is still there reads `status`, and a client that wants to know why reads the code.

On the client side the frame gets a `parseFrame` branch, a transcript block and a third word in
`stoppedNotice` — each a pure `transcript.ts` function with a node test, because no gate may need a browser
(HS-058's rule).

## D6 — the first pass waits for the boot's own re-provisioning

The startup reconcile moves every live row to `starting` and restarts its provisioning (HS-044), so a pass
taken the instant the task spawns would find no candidate at all. That pass is the one that matters most — a
session can already be over its cap when the machine comes back — so `run` waits until no row is `starting`
(100 ms poll), bounded at 60 s so a wedged row is a delay and not a policy that never runs. Then it sweeps and
ticks. It is also what makes the demo and the gate able to observe a pass in seconds rather than waiting the
spec's five minutes: the point of the interval is how stale a measurement can be, not how long a test must sit.

## D7 — D2's isolation claim is a test over the two `docker run` vectors

Item 14's last paragraph is D2: caches are per-session, no cache directory is shared. That is already true by
construction — a container mounts its own worktree, the project's `.git`, that repository's read-only hooks and
config, and its own pi-home — so the story's part is a witness, not code: a unit test over
`SessionContainer::run_args` for two sessions of one project, asserting that no writable host path is shared
except the project's `.git` (spec item 1's shared object store), that neither session's worktree or pi-home
contains the other's, and that the two per-session paths hold everything D2 names — `target/`, `node_modules/`
and pi's own cache. The one claim a daemon would add is that Docker honours the vector, which
`tests/container.rs` already measures for the vector this test reads.
