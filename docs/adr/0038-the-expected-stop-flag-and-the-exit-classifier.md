# The expected-stop flag is a column, and the classifier is a pure function with no observer yet

HS-030 is spec item 6's last paragraph: "The server records an expected-stop flag before every `docker stop
-t 10`. Exit with the flag set means `stopped`. Exit code 0 without the flag means `stopped` with reason
`pi_exit`. Any other exit means `error` with the last stderr lines." It replaces the earlier
clean-exit-means-ended rule, and `session.log` (item 15) is where the lines are kept.

## The flag is a `sessions` column that no client is sent

`sessions.expected_stop INTEGER NOT NULL DEFAULT 0` (migration `0004`, schema 4). A column rather than a
marker file in the session's directory, for three reasons:

- it must outlive the process that wrote it — a restart between the write and the exit is exactly the case
  where the row is the only witness, and HS-044's reconcile reads rows, not directories;
- it is written and read in one place, with the row's other facts, through the repository ADR-0009 made
  the only writer of `sessions`;
- a delete takes it with the row, and a row is what names a container in the first place.

It is **not** part of the `Session` value. `Session` *is* the API's session object — its serialisation test
pins fifteen keys in column order, and `tests/sessions_api.rs`/`session_list.rs`/`session_screen.rs` assert
the body against the same list — and phase 1b's own rule is that nothing in it may require a UI or API
change. So the flag is read and written by two repository methods, like `touch`, `set_worktree` and
`add_usage`: row facts that are not the session's representation.

**`take_expected_stop` is one compare-and-swap**, `UPDATE … SET expected_stop = 0 WHERE id = ?1 AND
expected_stop = 1`, and it answers "did this statement change anything". The obvious spelling — `…
RETURNING expected_stop` — was written first and is **wrong**: SQLite's `RETURNING` on an `UPDATE` answers
the row **after** the write (measured: a flag of `1` updated to `0` returned `0`), and PostgreSQL's `OLD`
has no SQLite spelling. Reading and then clearing in two statements would leave a window in which a second
observer sees the same flag and calls a crash a stop we asked for.

## One routine records the flag, and three policies use it

`src/stop.rs`'s `Stopper` is `set_expected_stop(…, true)` followed by the backend's `stop`, and it is the
**only** production path to a `docker stop`: the sweeper (HS-021), the create's eviction (HS-020) and the
abort ladder's forced rung (HS-016/022/023) all go through it. Three copies of the write is two copies a
test cannot see, and the forgotten one turns an ordinary eviction into an `error` row.

The flag is the **server's**, not the backend's: a backend may emit frames but may not write a row
(ADR-0019), and a backend that wrote this one would need a `SessionRepository`. The order — flag, then call
— is the story's first acceptance line. It is a property of the routine's two lines and is **not**
observable from outside it; the mutation that swaps them stays green, and that is recorded here rather than
defended by a test that cannot fail. What *is* proved: each of the three policies leaves the flag set
(`tests/exit.rs`), so deleting the write reddens all three.

`Stopper` also gives the sweeper a `Stopper` instead of a `Backends` and the ladder a `stopper()` beside its
`transitioner()`: the value carries the two handles the write needs, so a caller cannot have the stop
without the record.

## A clean exit is its own table row

The table (ADR-0016) gains `Trigger::PiExit`: `starting|idle|running → stopped`. Item 8's table has no row
for a clean exit — its "container exits unexpectedly → `error`" cell is what item 6 *replaces* for code 0 —
so the row is this story's amendment. `Trigger::PiExit`'s word and the reason it records are both `pi_exit`,
the way `idle_stop`'s are both `idle_stop`. `UnexpectedExit` keeps its row and still reaches `error`; it is
now the failure arm only.

## `src/exit.rs`: a pure classifier, and the one entry point that applies a verdict

| The flag | The code | The session |
| --- | --- | --- |
| set | any | `stopped` — the row the stop's caller already moved; `Exits::record` writes nothing |
| clear | 0 | `stopped`, reason `pi_exit` (`Trigger::PiExit`) |
| clear | ≠ 0 | `error`, reason = the last five non-empty stderr lines (`Trigger::UnexpectedExit`) |

`classify` is that table as a function with no I/O, so its four cells are unit tests rather than a container:
the story asks for exactly that, and it is the part HS-031 can call without owning any state. The reason is
bounded at `STDERR_LINES = 5` because the row's reason is read on a phone; a process that printed nothing
gets a sentence naming its exit code instead of an empty reason.

`Exits::record(session, code, stderr)` consumes the flag, classifies, appends `stderr` to
`<data>/sessions/<id>/session.log` (append-only, the lines verbatim), and applies the verdict through the one
state function. Every failure is one `warn` line: the process has already stopped, so the row's reason is
what an operator needs and a write that failed is not a second incident.

The log is the session's **stderr plus the server's own launch line**, and nothing else. Item 15's
"`session.log` holds stderr only" is what keeps a *tool's* output out of it (HS-033 gives each tool call its
own file); HS-031 puts the launch line there because "which command did we run" is the first question a
failed launch asks and there is no row column for it. The writer refuses an id that is not a bare path
segment, so the one place in the crate that turns a string into a directory cannot be used to write outside
`<data>/sessions/`.

**The row's reason is one line; the log is the complete record.** A failure's reason is the last five
non-empty stderr lines joined by newlines — the first `status_reason` in the tree that is not a word or a
sentence — so the page renders a *headline* rather than the wall: `transcript.ts`'s `stateText` shows the
last of those lines, clipped, and `session.log` keeps all of it. That is a rendering rule and not an API
change: the row says what happened, and only the badge decides how much of it fits.

## What is not built, and which story owns it

**Nothing observes a container's exit in this build**, so `Exits::record` has no production caller yet:

- the container runs **detached** on `sleep infinity` (ADR-0036) until HS-031 attaches pi's stdio, and a
  detached container's stdout is gone, so there is no exit code and no stderr to read;
- `--rm` removes the container a moment after it stops (measured, ADR-0037), so there is nothing left to
  `inspect` or `docker logs` afterwards;
- a Docker-backed create still ends `error` at the **pi** step, so no Docker session reaches `idle`, let
  alone a turn.

A watcher (`docker events`, a `docker wait` raced against `--rm`) would be a second mechanism HS-031 deletes
when it attaches the process it is actually supervising. So HS-030 owns **the decision, the flag and the
record**; **HS-031 owns the observation** and is the caller of `Exits::record`.

**And one constraint HS-031 inherits, which is not reachable in this build.** The flag is cleared by a new
container's provisioning, but a forced rung **resumes immediately** (`Ladder::climb` ends the turn and then
brings the session back), so the resume's container step can clear the flag *before* the stopped container's
exit is recorded. `classify` then reads "no flag" and turns an `idle` row `error` over a stop the server
asked for. The production ordering HS-031 must keep is therefore: **record the previous container's exit
before the next container is provisioned** — await the supervisor's report between the rungs and the resume
— or key the flag to the container's incarnation rather than to the session. `provisioner.rs`'s clear cites
this, and `tests/exit.rs`'s forced-rung test uses `end_turn` rather than the production `climb` for exactly
that reason: the ladder's own resume is what would reorder it, and there is nothing to observe today. The demo is therefore
half-showable — the flag from the shipped artifact, the three outcomes from `tests/exit.rs` — which is the
same honesty ADR-0029 applied to HS-025's `ahead`.

Two consequences recorded rather than papered over:

- **`AppState::exits()` is not added.** A wiring helper with no production caller is dead wiring; HS-031 adds
  it with the caller.
- **The log writer is private to `src/exit.rs`.** HS-031 needs it for the launch line and for streaming
  stderr; it will lift it then, and what it must not do is append the same lines twice — `record` writes
  what it was given.

## What a reviewer should be able to check

- `tests/exit.rs` has one flag test per production stop site (the sweeper's `sweep`, an HTTP create that
  evicts, the ladder's forced rung with a spent grace), one test that a provision clears it, one per outcome,
  and one for "taken once". They assert the *post-condition* (the stop recorded itself), not the ordering:
  see the paragraph above. Removing the flag write reddens all four flag tests; making `classify` ignore the
  flag reddens four (two lib unit tests, `the_flag_is_taken_once…` and `an_expected_exit_leaves_a_live_row_alone`);
  pointing `PiExit` at `error` reddens the clean one; deleting the provisioner's clear reddens the resume
  one. All four mutations were run, and the numbers above are what the runs printed.
- The "expected exit" pair is split on purpose: `an_expected_exit_leaves_a_live_row_alone` is the
  discriminating one (from `stopped` both classifier arms are no-ops, so the production-shaped test that
  follows it would pass with `classify` ignoring the flag entirely).
- `src/exit.rs`'s unit tests cover the four cells and the tail's edges (fewer lines than the bound, blank
  lines, nothing printed at all).
- `case_exit_classification` is the artifact's half: schema 4's column, an eviction's flag read out of
  `app.db` with `python3`'s `sqlite3`, and a resume clearing it.
