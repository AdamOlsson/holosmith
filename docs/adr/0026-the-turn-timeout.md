# The turn timeout is the turn's own deadline

This is spec item 13's first row: `turn_timeout_min` (45 by default) means "abort the turn using the
ladder in item 16". Three things had to be decided — where the clock lives, what the ladder is allowed to
do from inside the runner, and which word the row ends up carrying.

## The clock is the turn runner's, not a sweeper's

The obvious shape is a second sweeper: poll `running` rows, climb the ladder on the ones that have been
running too long. It does not work, and the reason is a fact about the row: **there is no per-turn
timestamp on it.** `last_active_at` is *activity*, and the steer moves it, so a watcher reading that
column would let a session that is steered every few minutes run for ever. The alternative is a new column
(`turn_started_at`), a migration, a schema bump and a writer to keep it true — machinery for a value that
one task already knows exactly.

`Turn::run` starts the deadline, so it is per turn by construction: a settled turn simply has no clock to
reset, a steer cannot extend one, and a *new* turn gets a fresh one because it is a new call. The
acceptance's third bullet is therefore not something enforced; it is what the shape does. `0` is the off
switch and builds no timer at all (`turn::timeout`), the same reading gave `idle_stop_min = 0`
(ADR-0025) — a key the operator turned off should cost nothing.

## Rung 1 needs the turn future alive, and rung 2 needs the ladder to finish

A deadline that simply dropped the turn future and climbed the ladder would be the server-side
cancellation ADR-0020 rejected: rung 1 *asks* the agent, and the agent's answer arrives as the turn
future resolving `TurnEnd::Aborted`. A future nobody polls cannot resolve, so every timeout would wait out
`abort_grace` and force-kill — the polite rung would never work.

The first draft raced the ladder and the turn inside one `select!`, which was wrong for the mirror reason:
`select!` drops the losing future, and the losing future here is the *climb*. A climb dropped after rung 2
has moved the row — between `running → stopped` and the resume that follows it — leaves a session stopped
with nobody left to bring it back, which is spec item 16 §2 broken outright. With the stub the window is
microseconds (its `stop` does not really await), so no test would have caught it; the `docker stop -t
5` makes it a live five-second window.

So the deadline's arm hands the ladder a **task of its own** through `Ladder::spawn` — the abort handler's
entry point — and then keeps awaiting the turn. Nothing is cancelled by either side: rung 1
still asks, the turn still answers `TurnEnd::Aborted`, rung 2 still stops and resumes, and the runner and
the ladder race each other exactly as an operator's abort and the runner always have (ADR-0020's two
owners, decided by the table). ADR-0020 gained an "Amended by ADR-0026" note for the one sentence this
changes: there is now a `select!` around `start_turn`, but it cancels nothing.

The cost is that a wedged backend that answers neither `abort` nor `stop` keeps the runner's task alive
until the turn resolves, and the row is the ladder's by then. One leaked task per wedged turn is a better
failure than a session nobody can resume.

## Both endings carry `turn_timeout`, and that is the point of the reason being a parameter

`Turn::finished` takes the reason the *runner* ended the turn, and only the timeout path passes one, so:

- rung 1 worked → the runner writes `running → idle` with reason `turn_timeout` instead of
  `abort_completed`. The row's `status_reason` and the `session.status` frame both say why. (An idle row
  carrying a reason is not new: the abort has done it since `abort_completed`.) A turn that *settles*
  after its deadline instead of stopping records the reason too, because it was asked to end because of
  the bound; on the ordinary path nothing is recorded.
- rung 2 fired → the ladder writes `running → stopped` with `turn_timeout`, stops the container and resumes
  the session (item 16's "and the server resumes it at once"), so the reason is the frame's and the row's
  for the moment in between.

This is the fifth reason word after `forced_abort`, `deleted`, `evicted` and `idle_stop`, and it is a reason
rather than a state for the same reason those are: what differs between one stop and another is why.

The page needs no sentence for it. `stoppedNotice` stays two words wide (ADR-0024's rule), because a
timed-out turn is resumed immediately — the operator sees an `idle` row whose badge says
`idle · turn_timeout`, which is the badge's own `state · reason` form and not a reassurance anybody
invented.

## What this story left alone

**Not the token budget.** checks usage rather than elapsed time, and its trip arrives from an event
rather than a clock: it will climb the same ladder with `budget_exceeded`, and the same question — who
writes the ending, and with which word — will have to be answered for it separately.

**Not the abort endpoint.** `POST /abort` still records `forced_abort` on rung 2 and `abort_completed` on
rung 1; a timeout is not an operator's abort and does not claim to be.

**No per-session clock, no new column, no migration, no new config key, no sweeper.**

## Consequences

- `crates/holosmith/tests/turn_timeout.rs` proves, in six tests: a turn past its deadline ends `idle` with
  `turn_timeout`, the ladder asked (`clear_queue` + `abort`) and never force-stopped a turn that answered,
  and the transcript's `running → idle` frame carries the reason; a turn shorter than its deadline settles
  with no reason and no abort; an agent that ignores the abort is force-stopped, the `running → stopped`
  frame carries `turn_timeout`, and the row is **not left stopped** — the ladder's resume is waited for,
  which is what a climb cut in half would fail; a second, fast turn on the same session settles with no
  reason and inside the deadline, which a session-wide clock could not do; `None` aborts nothing; and
  `turn::timeout` maps `0` to `None`, `1` to a minute, the default 45 to 45 minutes and `u64::MAX` to a
  saturated duration.
- Two mutations were run: filtering the timeout away fails three tests, and dropping the reason in favour of
  `abort_completed` fails two.
- The gate adds `case_turn_timeout`. It cannot *fire* the deadline — `turn_timeout_min` is whole minutes and
  the stub's turn lasts about a second, so nothing on this machine can hold a turn open for a minute — so
  it proves what the artifact can: with `turn_timeout_min = 1` in the config, a prompt's turn settles and
  its `running → idle` frame carries `reason: null`. The firing itself is `tests/turn_timeout.rs`'s, and
  that is stated in the case's comment rather than glossed.
