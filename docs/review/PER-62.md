# Review — PER-62 (HS-030 exit classification)

Fixed point `91b376a`; reviewed at `9b79f75` by a read-only `reviewer` child running the `code-review`
skill, with the spec axis sourced from the Linear body, `docs/stories/phase-1b-docker-backend.md`'s HS-030
slice, spec items 6/8/15 and ADR-0038. Saved verbatim; the `## Resolution` table is appended afterwards.

## Review

**Verification basis.** Read `/tmp/holosmith/reviews/PER-62-diff.txt` (all 1666 lines), the plan, the ADR, and the tree at HEAD. This role has no shell, so nothing below is confirmed by a run; every claim is source-read, and the gates a supervisor must run are listed at the end.

### Correct (both axes)

- **Migration and schema discipline (ADR-0009).** `migrations/0004_expected_stop.sql:17` is a single trailing `ALTER TABLE … ADD COLUMN … NOT NULL DEFAULT 0`; `schema.rs:17` bumps the literal in the same commit; all three applied-set assertions move (`tests/db_migrations.rs:143,163,347`). No backfill needed, correct expand step.
- **The SQL stays in the repository.** Both statements live in `sessions/repository.rs:58,67-68` and go through `sessions()`; nothing else in `src/` names the column except `repository.rs` and the two call sites via the trait, so ADR-0009/0011's "the repository owns every statement" walk still holds.
- **No lock across an await.** `stop.rs:60-70` is a sync `record` whose guard dies before `backend.stop().await` (`stop.rs:53-56`); `exit.rs:175-181` and `provisioner.rs:157-166` do the same. Matches ADR-0012/0013.
- **The table amendment is complete and consistent.** `Trigger::PiExit` (`state.rs:64-70`), `ALL` 15 (`:79-93`), word `pi_exit` (`:112`), the row (`:236-241`), the spec-row-by-row test (`:552-563`), the `TABLE.len()` assertion (`:572`) and the `6x15` comment (`tests/state_machine.rs:96`) all agree; `Resume` is still the only two-row trigger, so `no_state_and_trigger_is_admitted_by_two_rows` is unaffected.
- **Every production stop path is the one routine.** `sweeper.rs:166-169`, `http/sessions.rs:495-498`, `ladder.rs:255-257`; and there is no other `docker stop` in the tree — delete is `docker rm -f` (`backend/docker.rs:445-460`), so spec item 6's "before every `docker stop -t 10`" is literally met.
- **The unobservable half is recorded, not papered over.** ADR-0038's "no production caller yet" boundary, the refusal to add `AppState::exits()`, and the half-showable demo are the ADR-0029 precedent applied correctly; `session.log` writing is confined to `<data>/sessions/<id>/session.log` with no path arithmetic outside it.
- **Counts that had to move did.** `GLOSSARY.md:156` trigger list, `:162` Stopped entry, `docs/stories/README.md:117` (37→38, and 38 is right — ADR 0001…0038 exist), `docs/build.md:168-171`, `scripts/verify.sh:317`.

---

## Standards — **PASS with findings**

**Finding — P2 — `crates/holosmith/tests/exit.rs:430-435`: the test named for `record` never calls `record`.**
`the_classifier_is_the_one_exit_record_uses` asserts three `classify(...)` results only. Its comment claims "so a change to the table in `record` fails here", but `record` has no table — it matches on `classify`'s answer (`src/exit.rs:141-168`) — so nothing about `record` is exercised. It duplicates `src/exit.rs`'s `a_stop_the_server_asked_for_is_expected_whatever_the_code` and friends (`src/exit.rs:254-306`) in an integration binary. *Smallest fix:* delete it, or make it drive `Exits::record` and assert the row.

**Finding — P2 — the file is declared stderr-only in three places and given a non-stderr writer in two.**
`src/exit.rs:25` "**`session.log` holds stderr only** (spec item 15)" and `:27` "HS-031 adds the launch line there"; `docs/adr/0038-…md:72` "the lines verbatim, **stderr only** — item 15" and `:97` "HS-031 needs it for the launch line". The two halves cannot both be true, and `docs/stories/phase-1b-docker-backend.md:119` makes the launch line an HS-031 acceptance bullet, while `GLOSSARY.md:266-271` ("Session log") omits it entirely. *Smallest fix:* one clause in `exit.rs:25-28`, ADR-0038:72 and the GLOSSARY entry saying the file is the session's stderr plus the server's own launch line, and that item 15's "stderr only" is what keeps *tool* output out.

**Finding — P3 — three test names claim an ordering they do not prove.**
`tests/exit.rs:150`, `:192`, `:232` are `…_flag_is_written_before_the_backend_is_asked_to_stop`, but each waits for the stub to have *seen* `Call::Stop` and then reads the flag — which passes whether the write came before or microseconds after the call. ADR-0038:41-44 says exactly this ("the mutation that swaps them stays green"). *Smallest fix:* rename to `the_<site>_records_the_flag` and leave the ordering claim to the routine and the ADR, or note in each doc comment that this is a post-condition.

**Finding — P3 — the ADR's mutation census undercounts by at least two.**
`docs/adr/0038-…md:105-106`: "making `classify` ignore the flag reddens two". It reddens four: `src/exit.rs`'s `a_stop_the_server_asked_for_is_expected_whatever_the_code` and `the_flag_cleared_after_a_stop_no_longer_excuses_the_next_exit`, plus `tests/exit.rs:431` and `:409` (`the_flag_is_taken_once…` fails at its `assert_eq!(row.status, Idle)`). Since the line ends "All four mutations were run", the number is stated as observed. *Smallest fix:* say "four" (and note `an_expected_exit…` stays green — see the Spec axis).

**Finding — P3 — two sentences still hand the flag to HS-030 as future work.**
`docs/build.md:392-393` "the *expected-stop* flag that makes a stop distinguishable from a crash is HS-030's" and `src/backend/docker.rs:404-405` "The *flag* … and the classification … are HS-030's". Both read as pending after this commit; neither says what the flag is or who writes it. `docs/build.md` is touched by this diff (only in the gate-cases paragraph). *Smallest fix:* one clause in each naming the `sessions.expected_stop` column and `stop::Stopper`.

**Finding — P3 — `src/exit.rs:236-241` joins an unvalidated id into a path.**
`session_dir(data_root, session_id)` is `pub`, is advertised in `GLOSSARY.md:270` as a machine name, and feeds `create_dir_all`/`OpenOptions` (`:203-211`), which follow symlinks. Today the id always comes from a row (16 hex, `repository.rs` id query), so there is no live escape; a `debug_assert!` or a `components()`/hex guard keeps HS-031's supervisor from being the first caller to pass something else.

---

## Spec — **PASS with findings**

**Finding — P2 — clearing the flag in the container step can consume it before the *previous* container's exit is recorded.**
The story clears on provisioning (`provisioner.rs:98-100`), but the ladder **resumes immediately** after the stop that set it: `ladder.rs:141` → `resume` (`:294-306`) → `provisioner().spawn(...)` → container step clears. The stop's own exit is classified by HS-031's supervisor, which is not ordered against that resume, so a `climb` (abort, turn timeout, budget) can reach `record(143)` *after* the flag is 0 — `classify` then answers `Failed` (`exit.rs:85-88`) and `UnexpectedExit` takes an `idle` row to `error` (`exit.rs:166`, row `state.rs:224-229`). The same hazard exists for any stop the operator resumes quickly (sweeper, eviction). It is unreachable in this build (no observer), which is why no test can catch it — but it is the one ordering the ADR asserts the *opposite* of, and HS-031 inherits it silently. Note the forced-rung test dodges it by using `end_turn` (`tests/exit.rs:232-259`) rather than production's `climb`. *Smallest fix (cheap, now):* record the constraint in ADR-0038's HS-031 section — "the previous container's exit must be recorded before the ladder's resume provisions, or `expected_stop` must be keyed to the container incarnation" — and cite it from `provisioner.rs:92-100`.

**Finding — P2 — `tests/exit.rs:379-406` cannot fail for the reason it names.**
`an_expected_exit_leaves_the_row_the_stop_already_moved` sets the flag on a row it has already driven to `stopped` (`:385-386`). From `stopped`, *both* arms are no-ops: `Exit::Expected` writes nothing, and `Exit::Failed` → `UnexpectedExit` is refused by the table (`state.rs:224-229`'s `from` set excludes `stopped`), so every assertion — status, `status_reason == "idle_stop"`, `stopped_at`, the log — holds with `classify` ignoring the flag entirely. The discriminating case exists at `:409-427` (`the_flag_is_taken_once…` asserts the row stays `idle`), so the acceptance bullet is proved, not this test. *Smallest fix:* drive the same assertion from a live row — flag on an `idle` session, `record(143, "terminated")`, assert `status` is still `idle` and `status_reason` unchanged — and keep the log/`stopped_at` half here.

**Finding — P2 — the new `status_reason` shape is multi-line and unbounded, and it lands unclipped in the badge the demo shows.**
Spec item 6 wants "the last stderr lines", so `exit.rs:113-119` joins up to five non-empty lines — the first reason in the tree that is neither a word nor a single sentence. That value is rendered whole by `stateText` (`web/src/main.ts:162-164`) into the list row (`:195`), the screen badge (`:956`), and the live paths (`:282`, `:936`, `:996`), where `.state` has `font-size`/`opacity`/`text-align` and no width or ellipsis (`web/src/style.css:72-76`). The plan's "No UI change … already distinguishable" is where the miss is: `error · <five lines of stack trace>` is the demo's third run. *Smallest fix:* clip in one place — first line plus `…` in `stateText`, one `npm test` case — or record the consequence in ADR-0038 as HS-031/HS-064's.

**Finding — P3 — `scripts/verify.sh:1867-1869` reads the flag before the stop task is guaranteed to have written it.**
`wait_for_stopped_id` polls only the row's `status` (`:1525-1540`); the eviction's stop — and therefore the flag write — is a spawned task that runs *after* the row moved (`http/sessions.rs:496-498`). So `check "$name/evicted_flag" "1"` can read `0` on a slow scheduler. The Rust equivalent does wait (`tests/exit.rs:186-189` waits for `Call::Stop`), so the gate is the weaker half. *Smallest fix:* the bounded poll `wait_for_idle_id` already uses — `for _ in $(seq 1 50); do flag=$(expected_stop_of …); [ "$flag" = 1 ] && break; sleep 0.1; done`.

**Finding — P3 — `RESUME.md` is stale in five places this diff makes false.**
`:604` "`SCHEMA_VERSION` (= 3)"; `:516-518` the migrations list stopping at `0003`; `:109` "The six reason words are `forced_abort`, `deleted`, `evicted`, `idle_stop`, `turn_timeout` and `budget_exceeded`" (there are now seven, and `pi_exit` is a `stopped` reason); `:480`/`:1319` "49 cases / 367 assertions"; `:966` "44 cases / 284 assertions". The plan schedules this file "at the end", so treat it as a note that the story is not doc-complete until that lands — the reason-word list is the one a cold-start reader will trust.

**Finding — P3 — the UI decision "`stoppedNotice` says nothing for `pi_exit`" is unpinned.**
`web/test/transcript.test.ts:326-331` enumerates the reasons that must keep the dash (`forced_abort`, `deleted`, `turn_timeout`, `server_restart`); `pi_exit` is not among them, so a later sentence for it would land with no test to argue with — and `RESUME.md:930-933` states the rule ("adding a third word means a third sentence *and* a test that it is not another's"). *Smallest fix:* add `"pi_exit"` to that array.

---

### Verdicts

| Axis | Verdict | Worst finding |
| --- | --- | --- |
| Standards | **PASS with findings** — 2 P2, 4 P3 | `tests/exit.rs:430` (a test named for `record` that never calls it); the stderr-only contradiction (`src/exit.rs:25,27` vs `:72,97` in ADR-0038) |
| Spec | **PASS with findings** — 3 P2, 3 P3 | the flag cleared by the new container before the stop's own exit is recorded (`provisioner.rs:98-100` + `ladder.rs:141`), which HS-031 inherits as a misclassification the ADR does not record |

All five acceptance bullets are implemented at the seam they name, and the half the artifact cannot show is recorded in ADR-0038 rather than claimed. **Merge verdict: OK with notes.**

### Tests a supervisor must run (this role has no shell)

1. `cargo fmt --check`.
2. `cargo clippy --all-targets -- -D warnings` — `tests/exit.rs:430` is a `#[tokio::test]` with no `await`, and `tests/exit.rs:23-33` imports `Exit`, `Exits`, `LOG_FILE`, `PI_EXIT`, `classify`, `Stopper`, `Call`, `Served`, `Response`; all look used but only a build proves it.
3. `cargo test` — especially `--test exit`, `--test state_machine`, `--test db_migrations`, `--test session_repository`, and the lib's `sessions::state`/`exit` units.
4. `cd web && npm test` (unchanged by this diff, so it should be 34).
5. `scripts/verify.sh` on a machine **with** `python3` — expect **50 cases** (~375 assertions) and no `SKIP case_exit_classification` line; a `SKIP` means the artifact's half of the story was never shown, and `docs/build.md:168-171` would then be claiming something the run did not do.
## Resolution

Triaged against `a360fcd`. Every finding was checked against the tree rather than taken on trust; one
(the Spec P2 about the flag's clearing order) is recorded rather than fixed, because it cannot be fixed
without the observer HS-031 owns.

| Finding | Severity | Action |
| --- | --- | --- |
| **Standards** — `tests/exit.rs`'s `the_classifier_is_the_one_exit_record_uses` never calls `record`: it asserts three `classify` results and duplicates the lib's unit tests | P2 | **Fixed** — deleted. The pure cells are `src/exit.rs`'s, and `record`'s use of them is proved by the three outcome tests (which assert the row each arm produces). The name promised a table `record` does not have. |
| **Standards** — the session log is declared stderr-only in three places and given a non-stderr writer (the HS-031 launch line) in two | P2 | **Fixed** — `src/exit.rs`'s module doc, ADR-0038 and `GLOSSARY.md`'s **Session log** now say the same thing: the file is the session's stderr **plus the server's own launch line**, and item 15's "stderr only" is what keeps a *tool's* output out of it. |
| **Standards** — three test names claim an ordering nothing can observe (`…_written_before_the_backend_is_asked…`) | P3 | **Fixed** — renamed to `the_{idle_stop,eviction,forced_rung}_records_the_flag`, and the section's comment states that these are post-conditions and that the ordering is `Stopper`'s two lines (ADR-0038's "what is not proved"). |
| **Standards** — ADR-0038's mutation census says `classify`-ignores-the-flag reddens "two" | P3 | **Fixed** — it reddens **four**; re-measured with `cargo test --test exit --no-fail-fast` plus the lib run: `exit::tests::{a_stop_the_server_asked_for_is_expected_whatever_the_code, the_flag_cleared_after_a_stop_no_longer_excuses_the_next_exit}`, `the_flag_is_taken_once…` and `an_expected_exit_leaves_a_live_row_alone`. The ADR now says four and names them. |
| **Standards** — `docs/build.md` and `backend/docker.rs` still describe the flag as HS-030's future work | P3 | **Fixed** — both name `sessions.expected_stop` and `stop::Stopper` now, and so does `docker.rs`'s `stop_container` doc (the same stale sentence, found by grepping for the other two). |
| **Standards** — `exit::session_dir` joins an unvalidated id into a path it then creates | P3 | **Fixed** — `append_log` refuses an id that is not a bare path segment (`is_path_segment`, `warn` + nothing written), with a unit test asserting the ids that pass and seven that must not; `session_dir`'s doc says the id is a row's and that the writer is where an unsafe one is refused. |
| **Spec** — clearing the flag in the container step can consume it before the *previous* container's exit is recorded, and the ladder resumes immediately after its forced rung | P2 | **Recorded, not fixable here** — the constraint is now in ADR-0038's HS-031 section and cited from `provisioner.rs`'s clear: HS-031 must record the previous container's exit before the next container is provisioned, or key the flag to the container's incarnation. No test can reach it because nothing observes an exit yet; the reviewer is right that this is the one thing HS-031 could inherit silently. |
| **Spec** — `an_expected_exit_leaves_the_row_the_stop_already_moved` cannot fail for the reason it names (from `stopped` both classifier arms are no-ops) | P2 | **Fixed** — split. `an_expected_exit_leaves_a_live_row_alone` starts from an `idle` row and asserts it is still `idle` with no reason (mutation-proved: `classify` ignoring the flag reddens it), and `an_expected_exit_leaves_the_stopped_row_its_caller_moved` keeps the production shape and says in its comment that it cannot discriminate. |
| **Spec** — a multi-line `status_reason` lands unclipped in the badge the demo shows, and the plan claimed no UI change was needed | P2 | **Fixed** — the plan's claim was wrong and is superseded here. `stateText` moved into `transcript.ts` (where the renderer's rules are tested without a browser) and renders a reason as its **last** non-empty line, clipped at `REASON_MAX = 48` with an ellipsis; one line of `session.log` worth of truth in the badge, all of it in the file. Two node tests (`a badge shows a one-word reason as it is`, `a badge shows a multi-line reason's last line, clipped`). |
| **Spec** — `case_exit_classification` reads the flag before the spawned stop is guaranteed to have written it | P3 | **Fixed** — a bounded poll (50 × 100 ms) around `expected_stop_of`, with a comment naming ADR-0024's ordering. |
| **Spec** — `RESUME.md` is stale in five places the change makes false | P3 | **Fixed** — this session's refresh carries schema 4, `0004_expected_stop.sql` in the migrations list, seven reason words, and the 50-case gate; it is the hand-off file and is updated last, deliberately. |
| **Spec** — `pi_exit` is missing from the reasons `stoppedNotice` must stay silent for | P3 | **Fixed** — added to `web/test/transcript.test.ts`'s "no sentence is invented" list, so a future sentence for a clean exit has a test to argue with. |

Two things the review's "tests a supervisor must run" list asked for, run afterwards: `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`, `cd web && npm test` (36, up from 34) and
`npx tsc --noEmit`, then `scripts/verify.sh` on this machine **with** `python3` — 50 cases, no
`SKIP case_exit_classification` line.
