# Review — PER-48 (HS-016, abort with the full ladder)

- **Fixed point:** `700b157`
- **Reviewed change:** `d034237` (`700b157..HEAD`)
- **Reviewer:** a fresh `reviewer` subagent running `/home/adamolsson/.agents/skills/code-review/SKILL.md`,
  read-only, no shell. Its claims were checked by the orchestrator; the Resolution table below records
  what was fixed and what was dismissed with a citation.
- **Verdicts:** Standards — OK with notes; Spec — OK with notes.

This review is saved verbatim. The `## Resolution` section at the end was appended afterwards.


## Standards

Sources read: `AGENTS.md`, `PROMPT.md` (rules 2/73), `HolosmithSpec.md` §16 + the API table, `GLOSSARY.md`, `docs/adr/0012`, `0018`, `0019`, `0020` (treated as claims to check), `docs/build.md`, plus the working tree at `d034237` (no working-tree delta: `watchdog_diff` reports none).

**Correct**
- Route seam matches the established pattern, not a second one: `Routes::post` (`http/app.rs:167-178`) ⇒ `RouteKind::Mutating`, and the id is taken as `Result<Path<String>, PathRejection>` with `unwrap_or_default()` (`app.rs:414-421`), exactly as `GET /api/sessions/{id}` (`app.rs:364-370`) and prompt (`app.rs:407-412`) do — so a `%FF` segment stays inside ADR-0008's envelope.
- Lock discipline holds. `Ladder::row` is synchronous (`ladder.rs:236-239`) and every state change goes through `Transitioner::apply`, whose guard is scoped to a sync block (`sessions/state.rs:300-345`, "the guard is not `Send`, so it cannot outlive this scope"). Nothing in `ladder.rs`, `stub.rs:165-178` or `turn.rs:112` holds a guard across an `.await`.
- Broadcast after COMMIT is preserved by construction: the ladder never appends an event itself, it calls `apply` (write, then `events.append`, `state.rs:346-355`).
- The seq rule (`sessions.last_seq`, not `MAX(seq)`) and the `src/**` rule are untouched; the new `src/ladder.rs` uses `Transitioner` rather than `set_status`, so `no_status_write_outside_the_one_function` (`state.rs:538-567`, whose non-vacuity assertion names both files) still passes over the new file, and the file contains no SQL for the `src/sql.rs` scan to miss.
- Docs match the code: `GLOSSARY.md:116-125` (grace default `10_000 ms` at `config/mod.rs:232`, `0s` refused at `config/mod.rs:1167-1170`), `FORCED_ABORT` at `ladder.rs:51`, ADR-0020's step table matches the actual order, `docs/stories/README.md` count is now Twenty, and the "**Superseded by ADR-00NN:**" annotation it adds to ADR-0019 follows the convention already in ADR-0012:27,74 and ADR-0018:48,90.
- ADR-0020's checkable claims hold: ten trait methods (`backend/mod.rs:164-217`), no `select!`/token/registry in the server (only `serve.rs` and `events/stream.rs` use `select!`), `steer`/`resume` have no production caller, `28` serve cases (`scripts/verify.sh:259-287`), and `start_turn`'s doc keeps the "backend emits, server decides" boundary (`backend/mod.rs:171-188`).

**Findings**
- **consider (P2)** — `StateError::Unreported` is classified as "the row could not be moved", contradicting the variant's documented contract. `ladder.rs:179-189`: `Err(error) => { // The row could not be moved, so nothing may be killed … return; }`, and `ladder.rs:216-224` returns before spawning the provisioner. But `sessions/state.rs:200-205` documents `Unreported` as "The state changed and the event that reports it did not reach the writer … **The row is already right, so this is a warning, not a rollback**", and both existing runners treat it as success-with-a-warning (`apply_logged`, `state.rs:376-390`). Consequence on a writer that has stopped: rung 2 leaves the row `stopped` with a live container and no resume, and `resume` leaves the row `starting` with nothing provisioning it. *Smallest fix:* match `Err(StateError::Unreported(_))` alongside `Ok(_)` in both places.
- **consider (P2)** — `abort_grace` does not cover rung 1's own calls. `ladder.rs:99-103` awaits `rung_one(...)` unbounded, and the deadline is created only afterwards (`ladder.rs:137`, `let deadline = Instant::now() + self.grace;`). Spec item 16 measures from the abort (`HolosmithSpec.md:267`, "If pi is not idle after `abort_grace`"), not from whenever the two RPCs return; a wedged `clear_queue`/`abort` RPC (the hang this story exists for) means rung 2 never fires. *Smallest fix:* create the deadline before `rung_one` and bound the two calls with `tokio::time::timeout(self.grace, …)`.
- **nit** — `stub.rs:165` `fn stopping(&self, session_id: &str, stop: bool) -> bool` is a flag argument whose result means two different things: `true` because it just set the flag (`:171-173`) versus "it was set and is now consumed" (`:174-177`). Baseline *Mysterious Name*. *Smallest fix:* `request_stop(id)` + `take_stop_request(id) -> bool`.
- **nit** — `stub.rs:310-311` (`// A turn starts unstopped: whatever a previous turn left here belongs to that turn` / `self.stopping(&session.id, false);`) can discard an abort issued between the prompt's transition and the turn loop's first check; production runs a multi-threaded runtime (`commands/serve.rs:152`), so the window is real there. Effect is stub-only (a clean abort degrades to a forced one after the grace). The `#[tokio::test]` runtime is current-thread, so the tests are not flaky.
- **nit** — `config/duration.rs:26-31` adds `pub fn millis()` whose only caller is the `From` impl four lines below (`duration.rs:36`); the field is private but the impl is in the same module. Public API for a private purpose.
- **nit** — ADR-0018:88-90 is now false and was not annotated the way ADR-0019 was: "**prompt is HS-015's, abort HS-016's** … so pressing one names the story that will wire it … **Superseded by ADR-0019:** the prompt is wired; abort and steer still name their story." Abort no longer names its story (`web/src/main.ts:562-590`). ADRs are canonical (`PROMPT.md:10`). *Smallest fix:* one `**Superseded by ADR-0020:** abort is wired too`.
- **nit** — `http/sessions.rs:388-395`'s doc comment was edited by this diff (line 388, `not_in_state`) yet keeps a sentence this story retires: "a session left in `running` with no runner has no way out until HS-016 adds abort".
- **nit** — `web/src/main.ts:569` names the abort handler `stop`, while `GLOSSARY.md:112-114` says for **Abort** "_Avoid_: Cancel, kill, **stop**_" and `SessionBackend::stop` is a different operation (rung 2's container stop). *Smallest fix:* `abortTurn`.
- **nit (Duplicated Code, judgement call)** — `Transitioner::new(Arc::clone(&self.database), self.events.clone())` is now in four places (`provisioner.rs:169`, `turn.rs:184`, `ladder.rs:258`, plus `AppState::transitioner`) and `Provisioner::new(...)` in `ladder.rs:245`. The repo endorses "each runner owns its handles", so report only.

**Standards verdict: OK with notes** — no should-fix; the two `consider` items are the dying-writer path and an unbounded rung 1.

## Spec

**Correct** — every acceptance bullet is implemented and proved by a test that fails if the behaviour goes:
- *202 immediately, progress as events*: `sessions::abort` decides and spawns before building the answer (`http/sessions.rs:443-457`, "Spawned after the decision and before the answer is built"); the test asserts the 202 body is still `"status": "running"` (`tests/abort.rs:180-190`).
- *Rung 1 order*: `tail == [StartTurn, ClearQueue, Abort]` plus "the forced rung must not fire" (`tests/abort.rs:200-215`); `case_abort` proves it in the shipped artifact (`scripts/verify.sh:795-843`).
- *Rung 2*: `impatient_grace(150)` fed through `AppState.config` proves `abort_grace` is the config's (`app.rs:99-108`); `running→stopped` with `FORCED_ABORT` (`tests/abort.rs:272-286`), then `Stop`, then the four provisioning steps, ending `idle` with the same id and `status_reason: None`.
- *Abort completing normally*: `running→idle` on `abort_completed` (`turn.rs:84-97`, `tests/abort.rs:196-198`).
- *Shared routine, parameterised reason*: `Ladder` + `climb(reason)`, proved with `turn_timeout` (`tests/abort.rs:305-330`).
- *A test drives the stub to ignore rung 1*: `ignoring_abort()` (`stub.rs:151-162`) and the end-to-end test above — the demo is satisfied (`abort_completed` vs `forced_abort`).
- *No scope creep*: no steer route, no `POST /resume`, no delete, no timeout/budget caller, Docker still refuses every operation (`docker.rs:59-62`). Wiring the abort button replaces a placeholder note and mirrors what HS-015 did for the prompt (`main.ts:530-532`) — in-slice.

**Findings**
- **consider (P2)** — the parameterised reason reaches only rung 2, and ADR-0020 says otherwise. `turn.rs:37` `pub const ABORT_COMPLETED` and `turn.rs:112` hardcode rung 1's ending, while ADR-0020 states "HS-022's turn timeout and HS-023's budget climb the same two rungs **and record their own word** — `turn_timeout`, `budget_exceeded`". For a timeout whose rung 1 succeeds (the normal case) the recorded reason is `abort_completed`, which cannot satisfy HS-022's bullet 2 (`docs/stories/phase-1-session-lifecycle-stub.md:268`). `GLOSSARY.md:117` is careful ("the reason **rung 2** records is the caller's"); the ADR is not. *Smallest fix:* one sentence in ADR-0020 that rung 1's ending is always `abort_completed` and HS-022/HS-023 must plumb their word through `Turn`.
- **nit** — `tests/abort.rs:383-385` documents a check it does not make: "A session a turn is already running for is `running`, which is the prompt's own refusal code — checked here because both endpoints read the same table", immediately followed only by `idle_served.finish()`. Nothing is asserted.
- **nit** — `tests/abort.rs:298-309` accepts either `stopped→starting` with reason `"resume"` **or** reason `null`, but ADR-0020 settles it ("the resume's transition carries no reason, so a session that came back from a forced abort is `idle` with `reason: null`"). The `||` would pass if a reason were recorded. Assert the null form only.
- **nit** — ADR-0020 Consequences says "`src/backend/stub.rs` grows three tests' worth of behaviour"; it is one test with three phases (`stub.rs:517-566`).

**Spec verdict: OK with notes** — all six bullets plus the demo are implemented and proved; one ADR sentence overstates what the parameter buys the next two stories, and two test comments/assertions are looser than their own documentation.

**Tests a supervisor must run (this role has no shell).** `. ~/.pi/toolchains/env.sh && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cd ../web && npm test`, then `scripts/verify.sh` (expect 28 cases). Nothing below is confirmed by a run; every claim above is read from source.
## Resolution

Every finding was triaged. "Fixed" means the code or doc changed in `8ba1ba2`'s parent commit (the
fix commit for this story); "dismissed" carries the citation that makes it not a defect.

| # | Axis | Finding | Verdict |
| --- | --- | --- | --- |
| 1 | Standards, consider (P2) | `StateError::Unreported` was treated as "the row could not be moved" in rung 2 and in the resume, so a dying writer left a `stopped` row with a live container and a `starting` row with nothing provisioning it | **Fixed.** Both arms now treat `Unreported` as success, as `apply_logged` does and as `state.rs` documents the variant ("the row is already right, so this is a warning, not a rollback"): the container is still killed and the provisioner still spawned |
| 2 | Standards, consider (P2) | `abort_grace` did not cover rung 1's own calls: the deadline was created after they returned, so a wedged `clear_queue`/`abort` meant rung 2 never fired — the very hang the story exists for | **Fixed.** The deadline is created in `climb` before rung 1 and each call is bounded with `tokio::time::timeout_at(deadline, …)`, so the grace is measured from the abort (spec item 16: "If pi is not idle after `abort_grace`") |
| 3 | Standards, nit | `fn stopping(&self, id, stop: bool) -> bool` was a flag argument with two meanings | **Fixed.** Split into `request_stop` and `take_stop_request`, each with one meaning |
| 4 | Standards, nit | Clearing the stop request at the start of `start_turn` can discard a stop issued between the prompt's 202 and the loop's first check | **Dismissed and documented.** The window is real on the multi-threaded runtime, but the trade is deliberate: keeping the request instead would abort the *next* turn of the session instantly, which is wrong and instant, while dropping it degrades a clean abort into a forced one after the grace, which is slower but still correct. The comment in `start_turn` now says exactly that |
| 5 | Standards, nit | `Duration::millis()` was public with only the `From` impl as caller | **Fixed.** Private; the `From<Duration> for std::time::Duration` impl is the public surface |
| 6 | Standards, nit | ADR-0018's "abort HS-016's … abort and steer still name their story" is now false and lacked the superseding note | **Fixed.** `ADR-0018` carries "**Superseded by ADR-0020:** abort is wired too; only steer still names its story." |
| 7 | Standards, nit | The prompt handler's doc still said a `running` row "has no way out until HS-016 adds abort" | **Fixed.** The paragraph now says the ladder can end such a row, which is what makes the spawn order defensible |
| 8 | Standards, nit | `web/src/main.ts` named the abort handler `stop`, which the glossary lists under **Abort**'s *Avoid* and which is a different operation (`SessionBackend::stop`) | **Fixed.** Renamed `abortTurn` |
| 9 | Standards, nit | `Transitioner::new(...)` is now repeated in four places | **Dismissed.** Each runner owning its two handles is the shipping design (ADR-0019's runner shape, ADR-0016's "carries the two handles so a caller cannot have one without the other"); extracting a builder would be a fifth place, not one fewer |
| 10 | Spec, consider (P2) | The parameterised reason reaches only rung 2, while ADR-0020 claimed HS-022/HS-023 "record their own word" on the ladder | **Fixed in the ADR, deferred in code.** ADR-0020 now states plainly that rung 1's ending is always `abort_completed`, that the runner is not told why, and that HS-022/HS-023 must plumb their word through `Turn` (recorded as a follow-up in the Linear issue). This story's acceptance is the parameterised ladder, which is true; HS-022's own acceptance is HS-022's to prove |
| 11 | Spec, nit | `tests/abort.rs` documented a check it did not make ("both endpoints read the same table") | **Fixed.** The stale comment is gone; the behaviour it claimed is already covered by `tests/turns.rs` |
| 12 | Spec, nit | The rung-2 test accepted `stopped → starting` with reason `"resume"` *or* `null` | **Fixed.** It asserts the `null` form only, which is what ADR-0020 settles |
| 13 | Spec, nit | ADR-0020 said the stub "grows three tests' worth of behaviour" | **Fixed.** "one test in three phases" |

Gates re-run after the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` (208 lib + 216 across nineteen integration binaries, all green), `cd web && npm test`
(15), `scripts/verify.sh` (28 cases + the CORS sweep, 111 assertions).
