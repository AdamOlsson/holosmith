# The expiry is the delete flow's own second half on a daily tick

This is spec item 15's retention bullet: "a daily job deletes sessions that have been stopped for more than
7 days (config `stopped_ttl_days`, default 7), using the delete flow from item 9. A snapshot runs first; if it
fails, the session is skipped and retried the next day. Sessions in error are never auto-deleted. […] The
Sessions screen shows the days remaining on each stopped session." The key, the column and the flow all
existed; what did not exist was a caller that is not a request. Six things here were open, and the first is
the one a later policy will copy.

## D1 — a module and a task of its own, no off switch, an immediate first pass

`src/expiry.rs`, `AppState::expiry() -> Expiry`, spawned in `commands/serve.rs` beside the other three
policies and aborted with them. `TICK` is 24 hours, a constant for ADR-0025's reason: the spec's config list is
exhaustive, and the *day* is the policy itself — item 15 names `stopped_ttl_days` as the key where it names the
interval as prose. No new config key, no new validation rule, no golden-file move.

**There is no off switch, and that is a decision rather than an omission.** `stopped_ttl_days` already refuses
`0` (`config/mod.rs`), so a `0` here would mean "remove every stopped session the moment it stops", not
"never" — the opposite of what an operator setting `0` would want, and the one value a destroying policy must
not guess about. `AppState::expiry()` therefore answers `Expiry` and not `Option<Expiry>`, unlike `sweeper()`
and `quota()`, whose keys do have a `0`-means-off. An operator who wants no expiry raises the number.

**The first pass is immediate**, and the disk cap's `SETTLE` gate (ADR-0069 D5) deliberately does *not*
transfer. That gate exists because the startup reconcile moves every **live** row to `starting` and restarts
its provisioning, so a pass taken then measures nothing. A `stopped` row is not live, is not reconciled and is
not re-provisioned, so a pass taken the instant the task spawns already sees every candidate. The consequence
is the one that matters for a daily policy: it is *observable* — backdate a row, restart, read the row's
disappearance — instead of a twenty-four-hour wait, which is how both the demo and `case_stopped_expiry` see
it.

## D2 — the delete flow grows a second half, and both callers use it

`http::sessions::deletion` keeps steps 1–2 of ADR-0023 (the row, the `deleting` short-circuit, ending a
running turn) and then delegates to a new `pub(crate)`

```rust
complete_deletion(state, current: Session, trigger: Trigger, force: bool, landed: Option<&str>)
    -> Result<DeleteAnswer, ApiError>
```

which is the sequence that was already there, unchanged: `backend.snapshot` → the keep-rules
(`retained_for` → `Worktree::retained`, ADR-0052/ADR-0065) → the transition → `Deleter::spawn`. The name is
re-exported from `http/mod.rs` so the job can reach it without the module being opened to the crate.

"Using the delete flow from item 9" is exactly this: the snapshot before anything is removed, the branch and
the `refs/holosmith/wip/<id>` that survive (in `Worktree::remove`, which is told what not to delete), the row
*and its event log* removed in one transaction (`Deleter::remove_row`), and the session's streams closed. A
second implementation would be those four promises written twice, and the day they disagreed one of them would
be wrong about somebody's work. So the story's part is a caller and one log line, not a removal.

The snapshot's failure stays what it is — `409 unsaved_work` — and the two callers read it differently, which
is the whole of the story's third bullet: a *request* answers the refusal and its message names `?force=true`,
while a *policy* must not force ("discard the work" is a decision only an operator can make), so the job logs
an `info` line and moves on. Nothing about the row moves, so it is a candidate again tomorrow: **the retry is
the candidate set**, not a second attempt inside the same pass.

## D3 — `Trigger::Expire`, a table row of its own, and why the job may not use `Trigger::Delete`

`sessions/state.rs` gains one trigger and one row:

```text
stopped -(expire)-> deleting
```

The delete's own row stays `any -(delete)-> deleting`, because a request is entitled to end a running turn
first and `deletion` does that before it calls the second half. A job is entitled to nothing: a session the
operator **resumes** in the window between the pass's read and the flow's write has left `stopped`, and the
narrow `from` is what makes that a refusal rather than a session removed under them. That is ADR-0016's rule
kept — the table is the only place a legal move is decided, so the alternative (a comparison in the job) would
be a second authority — and it is the same kind of amendment `pi_exit` made (ADR-0038) and `disk_quota` made
(ADR-0069). The trigger has no reason word: the row is `deleting` and then gone, so nothing could read one.
`Trigger::ALL` is seventeen.

The pass re-reads the row too, and says so in a comment: it keeps the *log line* honest ("the session left
`stopped` after the pass read it"), not the safety. `tests/state_machine.rs` is what isolates the table row;
mutating it to `FromStates::Any` turns that test red.

## D4 — the candidate query is one statement, and the predicate is the story

`SessionRepository::expired_before(cutoff)` with a `const EXPIRED_BEFORE_FILTER`: `WHERE status = ?1 AND
stopped_at < ?2 ORDER BY stopped_at ASC, id ASC`, modelled on the idle stop's `idle_before`. One statement in
the repository, so the invariant is where every other sessions statement is.

`status = 'stopped'` *is* "sessions in `error` are never auto-deleted", and it keeps a `deleting` row out of a
job's hands as well — that removal is the delete flow's own in-flight work, and a second attempt would be two
removals of one session. It has a second guarantee behind it (a `stopped_at` that is `NULL` fails the
comparison, and `NULL` is what every other state has), and the two are independent on purpose:
`case_stopped_expiry` writes an `error` row **with** a stale `stopped_at` — a row no writer of this build can
produce — so only the predicate can keep it, which is an isolation `tests/expiry.rs` cannot construct.

All candidates, not one: a pass that removed one and left the rest for later passes would make the window a
function of how many sessions were forgotten (the idle stop's argument, unchanged).

## D5 — the cutoff is Rust-side and saturating

`clock::days(u64) -> Duration` beside `clock::minutes`, and a pure `expiry::cutoff(now, ttl_days)` =
`now.saturating_sub(min(ttl_secs, i64::MAX))`. Two saturations, for the reason `quota::over` and
`turn::timeout` give: `stopped_ttl_days` is a `u64` the config layer accepts without a ceiling, and a wrapped
window would turn "keep a stopped session for a week" into "remove it the moment it stops" — the one failure
mode a policy that destroys rows cannot have. A window longer than the epoch is a cutoff before 1970, which
matches every row: the honest answer for "keep them for ever".

## D6 — the row moves before the container, nothing holds the lock, and a failed removal is the operator's

`complete_deletion` applies the trigger and *then* spawns, which is ADR-0023's order, and `deleting` is not a
candidate (D4) — so a pass cannot pick up its own work in flight. The row is read in one short scope, the
guard is dropped, and `backend.snapshot` / `backend.delete` are subprocess and daemon work outside it: a pass
that held the mutex across a container's teardown would stall every request for that container's lifetime.

If the removal task fails, the row stays `deleting` and **the job does not retry it** — its candidate set is
`stopped` only. That is ADR-0023's own retry (`DELETE` again answers 202 and re-runs the task), and it is the
right one: a removal that fails is a machine failure, and the operator is the one who can see it in the log.

## D7 — the job emits nothing of its own

The flow already emits `session.status → deleting` (usually dropped, because the removal finishes inside the
writer's 25 ms tick), and item 9's "a delete is not a state change anyone else performs" holds. A
`deleted_by_expiry` reason would be a new vocabulary for a row that no longer exists, and nothing could read
it. The job's own trace is one `info` line per removal and one per skip.

## D8 — the days remaining is one number on the list and one pure function on the page

`GET /api/sessions` grows exactly one key, `stopped_ttl_days`, beside `network_default` and for the same
reason (ADR-0017): the number is the machine's, the screen needs both answers to draw itself, and one round
trip means no new endpoint. It is **not** an `expires_at` per row: the row already carries `stopped_at`, and
the client adds the two and compares them with its own clock — the only clock that can draw a countdown, and
one place to be wrong instead of two.

On the page it is `expiryText(stoppedAt, ttlDays, now)` in `transcript.ts`, a pure function with the clock
passed in, because a sentence is a rule and `npm test` is the only gate that can see one (`main.ts` keeps
`Date.now()`). `stoppedDetail(state, reason, expires)` joins it to `stoppedNotice(reason)` with `" · "`, and
stays the one function both the row's construction and the `session.status` patch path call (that story's
two-places rule). The window reaches the patch path through the DOM — `<ul id="session-list"
data-stopped-ttl-days="7">` — because `main.ts` keeps no state beyond the DOM; a row a create prepends reads
the same attribute.

The arithmetic is `ceil`, so a row with six days and twenty hours left reads "7 days" and a countdown does not
drop a day the moment a second passes; `remaining <= 0` is "past its retention window" (a row the job has not
reached yet, not an error), and one day is "within a day" rather than "in 1 days". **The wording never says
the session will be deleted** — the job snapshots first and skips a row whose snapshot fails, so a session on
that list can outlive its window with its work intact, and a sentence promising a delete would be a claim the
server refuses to make. The placeholder title (`RETAINED`, "days remaining arrives with ") is
deleted with the story it named.

## D9 — no migration, and the six reasons into `stopped` are one witness

`stopped_at` has been column 13 of `0002_sessions.sql`, and `set_status` has moved it since
ADR-0016 — `now` when the state becomes `stopped`, `NULL` otherwise. So acceptance bullet 1 is a *witness*,
not code, and the story's version of it is one test that drives `Transitioner::apply` through all six reasons
the machine stops a session by itself (`abort_ladder`/`forced_abort`, `evict`, `idle_stop`, `drain`/
`server_restart`, `pi_exit`, `disk_quota`) and asserts the clock is set each time and cleared by `Resume`. The
columns, `SCHEMA_VERSION` and `docs/schema/previous.sql` are untouched.
