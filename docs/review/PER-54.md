# Review — PER-54 (HS-022, turn timeout)

- **Issue:** PER-54, story HS-022 — "Turn timeout", from `docs/stories/phase-1-session-lifecycle-stub.md`.
- **Fixed point:** `5da2ae2` (the commit before this story's work). Diff artifact:
  `/tmp/holosmith/reviews/PER-54-diff.txt`.
- **Spec sources:** `HolosmithSpec.md` items 13 and 16; the HS-022 slice; the Linear body; the plan in
  `/tmp/holosmith/plans/PER-54-linear.md`; ADR-0026 (this change's own) plus ADR-0020 and ADR-0025.
- **Standards sources:** `AGENTS.md`, `GLOSSARY.md`, `docs/build.md`, `docs/adr/0001…0026`, and the sibling
  runners (`ladder.rs`, `deleter.rs`, `sweeper.rs`, `provisioner.rs`).
- **Verdict:** BLOCK on first reading — one P1 on each axis. Both were real; both are fixed below.

I've read the full diff artifact (`/tmp/holosmith/reviews/PER-54-diff.txt`, 887 lines) and the surrounding sources. Findings below are split by axis as the skill requires.

## Standards

**T1 · P1 · `docs/stories/README.md:143` — the ADR-0025 bullet is duplicated and glued onto one line by this diff.**
The hunk removes one line and adds a doubled one:

```
-- [ADR-0025](../adr/0025-the-idle-stop-sweeper.md) — the idle stop is a sweeper task … rather than the cap's.
+- [ADR-0025](../adr/0025-the-idle-stop-sweeper.md) — the idle stop is a sweeper task … rather than the cap's.- [ADR-0025](../adr/0025-the-idle-stop-sweeper.md) — the idle stop is a sweeper task … rather than the cap's.
+- [ADR-0026](../adr/0026-the-turn-timeout.md) — …
```

Read back from the file, line 143 is one physical line containing the whole ADR-0025 bullet twice, the second copy's link text welded onto the first sentence. The decisions index is the repo's map of every "hard to reverse" choice, and it now renders ADR-0025 twice inside a single bullet. This is plainly accidental (nothing else in the hunk needs it) and is a one-line fix: restore the single ADR-0025 bullet and keep ADR-0026 as its own line.

**T2 · P2 · `crates/holosmith/src/turn.rs:130` contradicts ADR-0020, which was not annotated.**
ADR-0020 ("Cancellation is the backend's") states, in the present tense and as a rule:

> There is no cancellation token in the server, **no `select!` around `start_turn`**, and no per-session registry of turn handles in `AppState`.

`Turn::run` now puts a `select!` immediately around `start_turn` (turn.rs:123-133). ADR-0026 argues the *spirit* survives ("the ladder still *asks* the backend, and the backend still decides how its turn ended"), which is a fair argument, but the ADR-0020 sentence a later reader will find is now false and nothing points them at ADR-0026. This repo already treats that as a defect and fixed it once before: `docs/review/PER-47.md:80` — "ADR-0018 is contradicted by this diff and never annotated → fixed. Two **Superseded by ADR-0019** lines in ADR-0018". Smallest fix: one line in ADR-0020's §"Cancellation is the backend's" — *"Amended by ADR-0026: `Turn::run` races `start_turn` against its own deadline; the ladder, not the server, still decides how the turn ended."*

**T3 · P2 · `docs/build.md:95-97` — the gate's config sentence is stale.**
> the config file carries `owner`, `max_live_sessions = 1`, `backend = "stub"` and `idle_stop_min = 1`: the `bad_owner` case, the create cases, provisioning and the idle stop are the only reasons that file exists.

This diff adds `turn_timeout_min = 1` to that file (verify.sh:1458) and touches this very document two paragraphs later, but the sentence that enumerates the file's keys and the reasons it exists still lists four keys and four reasons. Fix: add `turn_timeout_min = 1` and `case_turn_timeout` to the list.

**T4 · P2 · `scripts/verify.sh:7-14` — the file's own manifest (printed as `--help` by `usage()`) is not updated.**
> … the session's event stream, a prompt's turn, an abort down to `abort_completed`, a steer's own frame, a resume's two refusals, a delete … an idle session stopped by the sweeper with its reason …

`case_turn_timeout` is added to `serve_cases` (verify.sh:288) but not to this sentence, which `usage()` prints verbatim (`sed -n '2,/^$/…'`). Every previous story added its case here; fix by naming the timeout's quiet half in the list.

**T5 · P2 · `scripts/verify.sh:218-222` — the new budget comment states a number the code did not have, and hides the bound it now implies.**

> every line has its own three-second timeout, so a stalled stream fails the case instead of hanging the run. The line budget is 800 rather than **a hundred**: … so a budget sized for one turn would end the read before the frame it came for.

The removed line was `[ "$count" -lt 200 ] || break` — two hundred, not a hundred. And the old budget was not "sized for one turn": it was sized for the whole replay of the one session every case shares. The *change itself* is defensible — my read of the event volume says 200 was already marginal and `case_turn_timeout`'s two extra turns push the abort/steer replays past it — but the justification should say that, and the comment no longer states the resulting failure bound: 800 lines × 3 s is a **40-minute** worst case for a stalled stream, four times the 10 minutes the old budget allowed. Fix: "800 rather than 200" and one clause on the worst case; or keep 200 by giving `case_turn_timeout` its own session instead of two more turns on `$created_id`.

**T6 · nit · `crates/holosmith/src/clock.rs:1-6` — the module doc no longer describes the module.**
> This module is the only place that asks the operating system what time it is: the startup's migration timestamp (HS-004) and a create's `created_at`/`last_active_at` (HS-008).

`minutes()` neither asks the OS nor appears in that enumeration, so the new function has no home in the file's own stated job. The placement is defensible ("one spelling of a minute"); the sentence needs the third item.

**T7 · nit · `crates/holosmith/src/turn.rs:153` logs an `Option`, against the sweeper's precedent.**
`timeout_s = self.timeout.map(|limit| limit.as_secs())` renders `timeout_s=Some(60)` on a path where `self.timeout` is always `Some`, while the sibling policy logs a plain number (`sweeper.rs:146` `idle_min = self.idle_minutes()`). It also logs the field rather than the `limit` that fired. Fix: pass `limit` into `past_the_deadline` and log `timeout_s = limit.as_secs()`.

**T8 · nit · `crates/holosmith/tests/turn_timeout.rs:303-317` — the test's name promises a scenario it never builds.**
`a_turn_that_ignores_the_abort_still_ends_without_a_timeout_configured` uses `fast_stub().ignoring_abort()`, but with `None` no deadline exists, so nothing ever calls `abort`/`stop` and `ignoring_abort` changes nothing. The assertion (`!Abort && !Stop`) is still meaningful as the off-switch check; the decoration and the name are not. Fix: drop `.ignoring_abort()` and rename (`no_timeout_configured_aborts_nothing`).

**T9 · nit (smell, judgement call) · `crates/holosmith/src/turn.rs:263-269` duplicates `http/app.rs:107-113`.**
`Turn::ladder()` is a second wiring of `Ladder` with the same grace, and `Turn` now carries `grace` solely to hand it on. Given `Turn` cannot hold an `AppState`, this is reasonable and the comment is honest; noted only because ADR-0026's "the grace is the config's here exactly as it is in the abort handler … so a forced stop cannot take a different length of time" implies a guarantee the type does not give (`Turn::new` accepts any `Duration`).

## Spec

**S1 · P1 · `crates/holosmith/src/turn.rs:156-166` — the nested `select!` can cancel the ladder *after* rung 2 moved the row, leaving the session `stopped` and never resumed.**

```rust
let climb = ladder.climb(session, TURN_TIMEOUT);   // :157
tokio::pin!(climb);
tokio::select! {
    ended = &mut turn => self.finished(session, ended, Some(TURN_TIMEOUT)).await,
    () = &mut climb => {}
}
```

`climb` is `end_turn` *then* `resume` (ladder.rs:142-146 → `rung_two` → `backend.stop().await` → `resume`). `tokio::select!` drops every non-completed future before the winning branch's body runs, so whenever the turn future answers, the ladder is cut wherever it happened to be suspended. Two windows matter, and both are *after* the row has already been moved to `stopped` by `rung_two`:

- the ladder is suspended in `rung_two`'s `self.backend.stop(session).await` — the stop RPC is abandoned mid-flight (row says `stopped`, container possibly not), and `resume` never runs;
- the ladder is suspended in `resume`'s `Transitioner::apply(Trigger::Resume).await` — `resume` is abandoned.

Either way the row stays `stopped`, and the runner's own ending cannot save it: `abort_ended` applies `abort_completed` from `stopped`, the table refuses it, and that refusal is the *documented* `debug` line ("the abort ladder ended this turn instead"), so the failure is silent. This contradicts spec item 16 §2 ("The session becomes `stopped` with reason `forced_abort`, and the server resumes it at once"), ADR-0020's table row 2c, and ADR-0026's own claim that "if the ladder finishes first (a forced stop and its resume), the row is already the ladder's" — the code does not guarantee the ladder *finishes*; it can be cut in half. With the stub the window is microseconds (`stop` has no real await), which is why `an_agent_that_ignores_the_abort_is_force_stopped_with_the_reason` (`turn_timeout.rs:261`) gets away with it; with HS-024's `docker stop -t 5` both the "both branches ready" random choice and a genuinely suspended `stop` make it live.

Smallest fix — use the ladder's production entry point, which is what ADR-0020 built for exactly this, and keep awaiting the turn so rung 1 can still answer:

```rust
let ladder = self.ladder();
ladder.spawn(session.clone(), TURN_TIMEOUT);   // its own task: uncancellable here
let ended = turn.await;                        // rung 1 answers as TurnEnd::Aborted
self.finished(session, ended, Some(TURN_TIMEOUT)).await;
```

That keeps every property ADR-0026 argues for (the ladder still asks; the backend still decides `TurnEnd`; the runner still owns the `running → idle` ending) and reduces the forced path to the already-documented two-owner race with `/abort`. If the select is kept, the turn branch must `tokio::spawn` the remaining climb rather than drop it, which is strictly harder to get right.

**S2 · P2 · `crates/holosmith/tests/turn_timeout.rs:228-258` — `each_turn_gets_its_own_clock` cannot fail for the reason it names.**
The comment claims "A session-wide clock would let the second turn inherit the first one's spent time and end instantly — or never end at all", and the assertion is `assert_eq!(timeouts, 2, "one clock served both turns")`. But the test builds a **fresh runner for each turn** (`runner(&state, &stub, Some(TIMEOUT))` at :238 and :248), so no object, clock or deadline survives from the first turn to the second for a bug to carry over. And a session-wide deadline that had already expired would *still* produce exactly one `turn_timeout` for the second turn, so the count of 2 passes either way. The third acceptance bullet ("a settled turn resets the clock; the timeout is per turn, not per session") therefore has no test behind it — only the shape, which ADR-0026 acknowledges but the test file's own comment does not. Smallest fix: time the second turn (`let started = Instant::now()` … `assert!(started.elapsed() >= TIMEOUT)`) and, better, make the second turn a short one (`fast_stub()` with the same `TIMEOUT`) that must settle *with no reason* — a clock that inherited the first turn's spent time fails that immediately.

**S3 · P2 · `scripts/verify.sh:819-824` — the case's "per turn, not per session" claim is false.**
> That is also the "per turn, not per session" clock observed from outside: a session-wide deadline would have fired on the second prompt.

Both prompts in `case_turn_timeout` land about a second apart, on a session created a few seconds earlier in the same run. A session-wide deadline of one minute — whether measured from creation or from accumulated running time — would not have fired on the second prompt either, so the case cannot distinguish the two. (ADR-0026 does not claim it; `docs/build.md:134-136` does not repeat it — only this comment overclaims.) Fix: delete the clause, or make it true by leaving the gap that the case would need (which costs a minute, as `case_idle_stop` does).

**S4 · nit · `crates/holosmith/src/turn.rs:180-184` — a turn that settles after the deadline records no reason at all.**

```rust
Ok(TurnEnd::Settled) => {
    // A turn that finished in the same breath as its deadline finished, so the row says so
    // and no reason is invented for it.
    self.transition(session, Trigger::AgentSettled, None).await;
```

The comment covers the simultaneous case, but the code also covers a turn that settles *long after* the deadline (deadline fired, `rung_one` asked, the agent finished its script instead of aborting, `the_turn_ended` saw `idle` and never forced). Then neither `status_reason` nor the transcript ever carries `turn_timeout`, though only the `holosmith::turn` info line records that the bound was exceeded — a gap against "the reason reaches the transcript and the session's `status_reason`". Unreachable with today's stub (its `abort` flips the turn to `Aborted`), so this is report-only for HS-024; the fix, when it matters, is to pass `Some(TURN_TIMEOUT)` into the `Settled` arm as well.

## Verified by reading

| Check | Evidence |
| --- | --- |
| Clock lives in `Turn::run`, per turn, not on the row | `turn.rs:121-133`; no per-turn column exists (`sessions/repository.rs` has `last_active_at` only) |
| `0` builds no timer, and `u64` saturates | `turn.rs:68-70`; `clock.rs:26-28`; `tests/turn_timeout.rs:327-336` |
| The deadline reaches the ladder with reason `turn_timeout` | `turn.rs:157` `ladder.climb(session, TURN_TIMEOUT)`; `ladder.rs` `rung_two` applies `AbortLadder` with the caller's reason |
| Both endings carry the reason | `turn.rs:188` `abort_ended(session, reason.unwrap_or(ABORT_COMPLETED))`; ladder `rung_two` reason parameter |
| Config→runner wiring is single-pathed | `http/app.rs:83-89` is the only `Turn::new` in `src/`; `http/sessions.rs:606` is the only `spawn` |
| Gate's log assertion is meaningful (absent line would be visible) | `logging.rs:13` `DEFAULT_FILTER = "info"`; verify.sh sets no `HOLOSMITH_LOG`; `case_idle_stop` already asserts an `info` line the same way |
| The 800-line budget was probably necessary | 12 script events + 2 `session.status` per turn ≈ 4 SSE lines/event (stream.rs:84-87); ~5 turns on `$created_id` by `case_abort_stream` ⇒ ~250-300 lines > 200 |
| `turn_timeout` needs no page change | `web/test/transcript.test.ts:323` already lists it among the reasons that get `null` (pre-existing); ADR-0026's claim holds |
| New docs/ADR counts are right | 26 ADRs on disk; ADR-0026 exists and matches the artifact; `tests/turn_timeout.rs` has six `#[tokio::test]`s as the ADR claims |
| Story's Demo gap is owned, not new | ADR-0026 §Consequences ("nothing on this machine can hold a turn open for a minute") already covers it — not reported |
| No dead code introduced | `turn::timeout`, `clock::minutes`, `Ladder::climb`, `Turn::run`, `Turn::spawn` all have callers (`http/app.rs`, `http/sessions.rs`, `tests/turn_timeout.rs`) |

## Could not verify without running

- `cargo test` (all six new tests), `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- ADR-0026's two mutation claims ("filtering the timeout away fails three tests", "dropping the reason in favour of `abort_completed` fails two") — plausible from the assertions, unproven here.
- The real flake rate of `an_agent_that_ignores_the_abort_is_force_stopped_with_the_reason` under S1's cancellation window (the stub's instant `stop` hides it; I could not observe the ordering).
- A full `scripts/verify.sh` run: whether the 800-line budget is now sufficient *and* whether the two extra prompts changed any other case's timing (e.g. the process-wide one-minute sweeper vs `$created_id` sitting idle across the eleven cases between `case_create_session` and `case_abort_stream`).
- Whether `"status":"idle","status_reason":null` matches the row's serialized field order in the shipped artifact (read from the `Session` struct: `status` immediately precedes `status_reason`; not executed).
- Whether the review target was fully captured: `watchdog_diff` does not inspect committed ranges, so this review is bounded by the supplied artifact at fixed point `5da2ae2`.

**Verdict:** BLOCK — Spec: 4 findings, worst is S1 (the select cancels the ladder mid-rung-2, so a forced stop can leave the session `stopped` and never resumed, contrary to item 16 and ADR-0020's 2c); Standards: 9 findings, worst is T1 (the ADR-0025 bullet is duplicated and glued in `docs/stories/README.md:143`).

## Resolution

| # | Sev | Finding | Resolution |
| --- | --- | --- | --- |
| S1 | **P1** | The nested `select!` dropped the *climb* whenever the turn answered, so a ladder cut between rung 2's `running → stopped` and its resume could leave the session stopped forever — spec item 16 §2 broken, and silent because the runner's own ending is then a documented `debug` line | **Fixed, with the reviewer's suggestion.** The deadline's arm now calls `Ladder::spawn` — the abort handler's entry point — and then awaits the turn, so neither side is cancelled: rung 1 still asks and answers as `TurnEnd::Aborted`, rung 2 still stops and resumes, and the runner and the ladder race exactly as `/abort` and the runner always have. A new doc paragraph in `turn.rs` says why the branch shape was wrong. **Proved by a new assertion**: `an_agent_that_ignores_the_abort_is_force_stopped_with_the_reason` now waits until the row is no longer `stopped`, which is the ladder's resume — the thing a cut climb loses. |
| T1 | **P1** | `docs/stories/README.md:143` carried the ADR-0025 bullet twice, the second copy glued onto the first line | **Fixed.** One bullet, and ADR-0026 on its own line. The duplicate came from this session's scripted insertion, not from the story. |
| T2 | P2 | ADR-0020 says "no `select!` around `start_turn`" in the present tense; `Turn::run` now has one, and nothing pointed at ADR-0026 | **Fixed.** ADR-0020 gained an "Amended by ADR-0026 (HS-022)" paragraph saying what changed (when the ladder is asked) and what did not (no server-side token, no belief about a turn the agent is still running). ADR-0026's own section was rewritten for the `spawn` shape. |
| T3 | P2 | `docs/build.md`'s sentence enumerating the gate's config keys and the reasons it exists was stale — `turn_timeout_min = 1` and `case_turn_timeout` were missing | **Fixed.** Both named. |
| T4 | P2 | `scripts/verify.sh`'s header manifest (printed by `--help`) did not mention the new case | **Fixed.** "a timeout that does not fire on a short turn" is in the list. |
| T5 | P2 | The 800-line comment claimed the old budget was "a hundred" (it was 200), mis-stated why it was sized that way, and no longer named the resulting bound | **Fixed.** The comment says 800 rather than 200, why the log grew (four or five turns on the shared session by the abort and steer cases, 300-odd lines), and what the budget means (200 events; a stalled stream fails on one line's three-second timeout, so the budget is not the hang bound). |
| T6 | nit | `clock.rs`'s module doc enumerates what the module does and did not mention `minutes()` | **Fixed.** The doc names it and says what it is for. |
| T7 | nit | The timeout's log line printed `timeout_s=Some(60)` on a path where the value is always `Some` | **Fixed.** `past_the_deadline` takes `limit` and logs `timeout_s = limit.as_secs()`. |
| T8 | nit | `a_turn_that_ignores_the_abort_still_ends_without_a_timeout_configured` decorated itself with `.ignoring_abort()`, which changes nothing when no deadline exists | **Fixed.** Renamed `no_timeout_configured_aborts_nothing`, decorator dropped. |
| T9 | nit (smell) | `Turn::ladder()` is a second `Ladder` wiring, and its comment implied the grace is guaranteed to match the abort handler's | **Kept, comment corrected.** A `Turn` cannot hold an `AppState`; the note only claims what is true now — the grace is the field `AppState` gave this runner. |
| S2 | P2 | `each_turn_gets_its_own_clock` built a *fresh runner* per turn, so no clock could carry over and a count of two timeouts proved nothing about "per turn, not per session" | **Fixed.** The second turn is now a fast one under the same deadline: it must settle with no reason and inside `TIMEOUT`, which a clock that inherited the first turn's spent time could not do. |
| S3 | P2 | `case_turn_timeout`'s comment claimed the gate observed "per turn, not per session", which two prompts a second apart cannot show | **Fixed.** The claim is gone; the comment now says what the case can prove (two fresh runs of the runner, no reason recorded, no deadline line) and points at the test for the clock. |
| S4 | nit (report-only) | A turn that *settles* after its deadline recorded no reason at all | **Fixed now rather than later.** The timeout path passes `TURN_TIMEOUT` into the `Settled` arm too, so the reason reaches the row and the transcript whichever way the turn ended under a fired deadline; the ordinary path still records nothing. |
