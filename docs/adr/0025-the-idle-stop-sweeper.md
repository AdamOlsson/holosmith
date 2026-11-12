# The idle stop is a sweeper on a fixed tick

This is spec item 13's second row: `idle_stop_min` (default 30) means "an idle session becomes
`stopped`, freeing a slot". Nothing about that fits an endpoint — no client sends it, and no request
waits for it — so the story's real content is four small decisions about a task.

## A policy is a module with a pass, not a route

`crates/holosmith/src/sweeper.rs` holds `Sweeper { backend, database, events, idle_after }`, which is the
fifth handle of this shape after `Provisioner`, `Turn`, `Ladder` and `Deleter`: a `Clone` value built from
the state's four fields, `spawn()` for production, and a public method (`Sweeper::sweep`) that does the
work once so a test can await it instead of racing a spawned task. That public method is a pattern now,
not an accident — three reviews in a row have pointed at a test that had to `wait_until` a background task
when it could have awaited one.

`Sweeper::run` is `sweep`, then sleep, forever. The first pass is immediate, so a process that restarted
with quiet sessions applies the policy now rather than a tick from now. The task's handle is aborted after
`serve::run` returns: the drain and the **Drain** of item 10 are somebody else's, and a shutdown
must not wait for a tick to come round.

## The tick is a constant, not a config key

`TICK` is five seconds. A config key would have been easy — the config layer already has a `Duration`
kind — and it would have been wrong twice: the spec's config list is exhaustive (ADR-0006 spells every
key the machine holds), and how often a process looks at a column is not a machine setting. ADR-0015
already refused the same temptation for the stub's 200 ms pacing.

Five seconds is chosen against the policy's own unit. `idle_stop_min` is whole minutes, so the tick is
what bounds the *lateness* of a stop: at five seconds `idle_stop_min = 1` means "between one and about
one-and-a-tenth minutes", and anything coarser would make the key's smallest value a lie. Finer would be
free of meaning: the event writer already wakes every 25 ms, and the sweep is one indexed read.

That choice has a cost the gate now pays: `case_idle_stop` in `scripts/verify.sh` waits the minute out,
because there is no shorter lever and inventing one for the harness would be a second spelling of the
policy. It is stated in the case's own comment rather than hidden.

## `0` is the off switch

The config layer accepts any non-negative integer for `idle_stop_min` and its semantic pass has no floor
for it. the acceptance says in as many words that `0` disables `turn_timeout_min`; item 13 puts the
two keys in one table of runaway controls, so reading one as "disabled at zero" and the other as "stop
everything" would be two meanings for one number. `0` therefore disables the idle stop, and `0` is not an
error: an operator who does not want the policy should not have to argue with the config validator, and
should not pay for a task that sweeps and finds nothing.

`AppState::sweeper()` returns `Option<Sweeper>` — `None` at zero — so the decision lives in one place,
next to the four wirings it resembles. `serve` records which of the two happened as a `debug` line
(the bound and the tick when it runs), not as a ninth startup line: the eight banner lines are pinned
byte for byte by `tests/serve_cli.rs` because an operator reads them in `journalctl` with diagnostics
off, and a policy's own state is diagnostics, which `HOLOSMITH_LOG` decides whether to show. The line
an operator *does* get without asking is the one per stop, and that is the story's own bullet.

## One apply per candidate, the row before the container, and the container spawned

The candidate read is `SessionRepository::idle_before(cutoff)`: every `idle` row whose `last_active_at`
is older than `now - idle_stop_min`, quietest first. Every candidate, not one — `oldest_idle`
answers the cap's question ("whose slot can I have?") and has no time bound; a sweep that stopped one
session per tick would make a stop's latency a function of how many sessions the operator left alone.

The `status = 'idle'` predicate is the story's second bullet, and it is the query rather than a filter
afterwards, so the rule has one spelling. The table refuses the rest anyway — `running + idle_stop` is not
a row — which is why the test that ages a `running`, an `error` and a `deleting` session and sweeps
asserts the *outcome* rather than which of the two guards fired. Two guards for one invariant is right
here: the broom keeps a refused move out of the log's info line, and the table keeps a bug out of the row.

Per candidate: one `Transitioner::apply(IdleStop, Some(IDLE_STOP))`, then one `info` log line, then the
backend's `stop` **spawned**. ADR-0020's order and ADR-0024's shape, for their reasons — the row is what
frees the slot, so the sweep must not wait for container work (item 7), and one wedged backend must delay
nothing but its own stop: not the rest of the sweep, not the next tick. A stop that fails logs and is not
retried, because the operator can delete a session whose container outlived it.

A refused `apply` is an `info` line and not an error, the same call `evict_one` makes: a prompt arriving
between the read and the move is an ordinary race, and the row is left exactly as the prompt left it.

## The page says what happened, in its own words

`stoppedNotice` gains `idle_stop`: "stopped after sitting idle — nothing was deleted, resume it to
continue". Deliberately not the cap's sentence: an idle stop did not need the room, and telling the
operator it did would be a false claim about a machine that had plenty. What the two share is the promise
that matters — nothing was deleted, and a resume continues the session — and that is the same promise
because it is the same stop.

The sentence lives in `web/src/transcript.ts` and not in `main.ts` for the reason, and the review of
that story is why: the span it fills is written twice, once when a row is drawn and once when a
`session.status` frame turns a live row into a stopped one. The sweeper's stop arrives by the second path
— the operator is watching a session they left open — so one function is the whole point.

## What this story left alone

**Not the drain.** stops every session for a restart, and it will reuse nothing here: a drain is
one pass over all live sessions with its own reason, not a policy about quiet ones.

**Not the cap.** A sweep stops a session because it was quiet; a create stops one because the machine is
full. They share the table row's shape and the reason-word convention, and nothing else — the cap's
eviction stays inside the create's own flow (ADR-0024), and the sweep takes no lock the create holds.

**Not a stopped session's fate.** deletes sessions that have been `stopped` for `stopped_ttl_days`;
this story only produces more of them, and its `stopped_at` is the column will read.

## Consequences

- `crates/holosmith/tests/idle_stop.rs` proves, in eight tests: a quiet `idle` session goes `stopped` with
  `idle_stop`, `stopped_at` set, its worktree path untouched, the live count falling to zero, the backend
  asked to `Stop` and nothing else, and the transition in the event log; a session inside the bound
  survives a sweep; one sweep stops every quiet session rather than one; a `running`, `error`, `deleting`
  and `starting` row is untouched however old; the bound is the key in minutes, pinned to ninety seconds
  inside and a hundred and thirty past a two-minute bound so a ×1.5 mutation cannot pass; the candidate
  **read** returns nothing but `idle` rows, which is the query's own predicate and not the table's
  refusal; and `0` yields no sweeper while the default does.
- Two of those were proved by mutation: dropping the `last_active_at` comparison fails three tests,
  dropping the `status` predicate fails the candidate-read test, and `* 90` for `* 60` fails the bound's.
- **The stderr line per stop is the gate's to observe.** Nothing under `tests/` installs a tracing
  subscriber, so deleting the `info` line leaves `cargo test` green and only `scripts/verify.sh`'s
  `grep` of `serve.err` sees it. That is stated rather than papered over: a test-side capture would be a
  second observer of a diagnostic, and the story asks for the line to exist, not for two witnesses.
- `web/test/transcript.test.ts` adds a case for the new reason word and asserts it is not the cap's
  sentence; `npm test` is 22.
- `scripts/verify.sh` grows `case_idle_stop`, which waits the gate's one-minute bound out against the
  shipped artifact and asserts the row, the reason, the replayed transition, the sweeper's log line and
  the freed slot, and then removes what it made. The gate's config gains `idle_stop_min = 1`, which is
  **process-wide**: every case from `start_serve` on runs with a sweeper, so no later case may leave a
  session `idle` for a minute. The case is placed where the table is empty (just after
  `case_delete_session`) for that reason as well as for the cap of one.
- The reason vocabulary is now four words: `forced_abort`, `deleted`, `evicted`, `idle_stop`. that story's
  `turn_timeout` and the `budget_exceeded` join them.
