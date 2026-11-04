# PER-55 — HS-023, token budget

- **Issue:** PER-55, story HS-023 — "Token budget", from `docs/stories/phase-1-session-lifecycle-stub.md`.
- **Fixed point:** `f6ae924` (the commit before this story's work). Diff artifact:
  `/tmp/holosmith/reviews/PER-55-diff.txt` — 17 files, 1617 lines, including the staged untracked ones.
- **Spec sources:** `HolosmithSpec.md` item 13 and V3; the HS-023 slice; the Linear body; the plan in
  `/tmp/holosmith/plans/PER-55-linear.md`; ADR-0027 (this change's own) plus ADR-0013, ADR-0019 and
  ADR-0026.
- **Standards sources:** `AGENTS.md`, `GLOSSARY.md`, `docs/build.md`, `docs/stories/README.md`, the ADRs,
  and `RESUME.md`'s landmine list.
- **Verdicts:** Spec — no P1, one P2, two P3. Standards — no P1, eight P3. Merge — OK with notes.

The review below is verbatim, as the process requires: the body was not edited, and every finding is
answered in the Resolution at the end.

---

I've read the full brief, the 1617-line diff artifact (all 17 files), and every source file the diff names. Here is the two-axis review.

## Spec

**Correct (with evidence)**

- *Bullet 2 — "At `max_session_tokens` the turn is aborted through the shared ladder with reason `budget_exceeded`."* The climb is `Ladder::spawn` (`turn.rs:228-230`), not a `select!` branch, so it cannot be dropped — RESUME.md §6's landmine is honoured. Rung 2 fires only if the row is still `running` (`ladder.rs:196-201`), so a climb that loses the race to a settle cannot force-stop an `idle` session.
- *Both policies can't climb twice.* `fired: Option<&'static str>` guards both arms (`turn.rs:185-208`), and the exit is a single `return` inside the loop's turn arm (`turn.rs:181-184`), so the loop has one exit and a fired policy disables the other. `the_first_policy_to_fire_is_the_only_one_that_climbs` asserts exactly one `Call::Abort`.
- *The tally commits with the event.* `insert_batch` adds `add_usage` **inside** `unchecked_transaction` after the row insert (`events/repository.rs:110-123`); an early `Err` drops the `Transaction`, which rolls back, so a half-failed batch leaves neither row nor tally. `add_usage` returning `Ok` on a missing row is correct because `bump_last_seq` already dropped that event (`events/repository.rs:87-95`). The repository still owns every statement against `sessions` (`ADD_USAGE`/`SPEND` at `sessions/repository.rs:40-48`), and `spend()` follows the existing `live_count` idiom (`query_row` + `get(0)`).
- *Bullet 3 — unset means unlimited.* `budget: Option<u64>` with `None` building no poll (`turn.rs:128-131`, `until()`'s `pending()` at `turn.rs:108-113`), `0` still refused by the validator (`config/mod.rs:560-568`), and `an_unset_budget_never_ends_a_turn` proves the tally is still persisted without a ceiling.
- *Bullet 4 — no-usage contingency, exercised.* `StubBackend::omitting_usage()` is a constructor argument only (`stub.rs:182-195`), never a config key; `a_backend_that_reports_no_usage_cannot_trip_the_budget` runs it with a ceiling of 1. The screen's rule is one function used by both the build and the patch path (`web/src/transcript.ts:307-312`, called at `main.ts:882-886` and `main.ts:847`), and its inference is right: `settled === 0` is "no turn yet", `usage` is "reported", so neither the row's zeros nor a reload can confuse the two.
- *The three endings match the ADR.* `Settled` gets the last look (`turn.rs:300-310`), `Aborted` gets `fired` via `reason.unwrap_or(ABORT_COMPLETED)` (`turn.rs:311-315`), and `Err` keeps the backend's own text (`turn.rs:316-330`) — so an operator abort or an error is never relabelled, as ADR-0027 promises.
- *The gate case owns its state.* New `config-budget.toml` + `data-budget/` (`verify.sh:1413-1470`), needle `"reason":"budget_exceeded"` is unique so no earlier turn's replay can satisfy it (RESUME.md's stream landmine), and the row assertion matches the real JSON order (`sessions/mod.rs:216` ends `"usage_in":N,"usage_out":N}`). `case_token_budget` is last in `serve_cases` and `case_no_cors_header` runs over saved response files after the loop (`verify.sh:1472-1478`), so the restart leaves both later phases intact; `stop_serve` asserts exit 0 for whichever boot is live, so the second boot's SIGTERM is covered too.

**Findings**

- **P2 — the accumulation the design rests on is not proven, and ADR-0027 says it is.** `docs/adr/0027-the-token-budget.md:92`: "a tally from two turns adds up". No test has two usage-reporting turns on one session: `a_message_end_without_usage_adds_nothing_to_the_tally` (`tests/token_budget.rs:333-359`) has the second turn *omit* usage, and `a_turn_that_spends_the_budget_ends_with_the_reason` aborts the second before its `message.end`. Mutating `ADD_USAGE` (`sessions/repository.rs:43-44`) from `usage_in = usage_in + ?2` to `usage_in = ?2` — the exact regression the comment at `:40-42` warns about — leaves all six tests green. That is RESUME.md's "mutating the rule and watching the test fail is the only proof" plus its "a test's claim can be half-strength and still green", applied to spec item 13's "updated from the `usage` on `message.end`". *Smallest fix:* one test running two usage-reporting turns under no ceiling and asserting `(2400, 680)` (the stub mints a per-turn `msg-<n>`, so the second `message.end` is a genuine second add); then correct ADR-0027:89's "in five tests" to six/seventh.
- **P3 — the "disabled" contingency silently under-counts a mixed backend.** Story bullet: "If the backend reports no usage, the budget is disabled". A backend that reports usage for turn 1 and omits it for turn 2 leaves the tally at 1540 while the session spent far more; `0 >= ceiling` is false, so nothing trips (`events/repository.rs:110-123`, `turn.rs:261-280`). This is what plan item 4 chose, so the fix is a sentence rather than code: say in ADR-0027 that the budget counts *what was reported* and a partially-reporting backend under-counts, instead of calling it disabled.
- **P3 — GLOSSARY's uniqueness claim is falsifiable from the code.** `GLOSSARY.md:299`: "Unset is unlimited — it is the one key whose absence is a setting". `config/mod.rs:1442-1448` lists four optional keys (`owner`, `docker_host`, `hosts_seed`, `max_session_tokens`), each with a meaningful absence. *Smallest fix:* scope the claim — "the one key in item 13's table whose absence is the off switch, where `turn_timeout_min` and `idle_stop_min` use `0`" (which matches `GLOSSARY.md:125` and `:179`).

**Spec verdict:** no P1. One P2 (a missing test plus a doc claim that overstates the suite) and two P3s; the behaviour asked for by the four acceptance bullets is implemented and matches spec item 13 and V3.

## Standards

**Correct (with evidence)**

- One writer (ADR-0013): the tally is written only inside `insert_batch`'s transaction, and a broadcast subscriber was rejected for lag semantics — the reasoning is recorded, not implied.
- The ladder is shared with a parameterised reason (`ladder.rs:20-22`), `Transitioner::apply` is still the only state change (`turn.rs:339-345`), and `add_usage` is exempted from it with a stated reason (nothing about the state moves) — the right call.
- One kind name per event kind: the constants now live in `events/mod.rs:32-50` and the stub's six literals are gone; `TURN_STEER` moved there too rather than being respelled.
- One sentence, one function, both DOM paths: `budgetNotice` is drawn at `main.ts:897` and patched at `main.ts:847` through the same closure the screen built.
- No new npm dependency, no `awk`/`shellcheck` in `scripts/` (the new gate code uses `grep`/`sed`/`seq` only), no new config key, no migration. `docs/build.md`'s boot prose, the `serve-data-budget.out`/`.err` naming, `docs/stories/README.md`'s twenty-seven and the ADR-0027 link all match the code.

**Findings**

- **P3 — a doc comment was mangled into the code.** `sessions/repository.rs:294`: `/// Remove this session's row and its event log, in **one** transaction (HS-019).    ///` — the `spend` insertion merged the old line with a dangling `///` and left trailing whitespace. *Fix:* split the line.
- **P3 — the new helper is spliced into another function's comment.** `verify.sh:1089-1108`: `wait_for_reason_id` sits between `wait_for_gone`'s doc paragraph ("The removal is a background task's (HS-019)…") and `wait_for_gone` itself, so that paragraph now heads the wrong function and `wait_for_gone` is undocumented. *Fix:* move the insertion above the `# wait_for_gone …` block.
- **P3 — dead variable.** `verify.sh:63` and `:1482` assign `serve_out`; nothing ever reads it (only `serve_err` is read). *Fix:* drop it, or `cat "$serve_out"` in the `serve/banner` failure detail so it has a reader.
- **P3 — `WRITER_TICK` restates another module's constant.** `turn.rs:92` hardcodes 35 ms as "`WriterOptions::default().commit_interval` is 25 ms, plus a little", but `events/mod.rs:147-155` documents that interval as configurable ("25 ms in production; short in tests"). A test with a different interval silently makes the second look meaningless. *Fix:* derive it (`WriterOptions::default().commit_interval + margin`) or name it in `events`.
- **P3 — the second look's comment understates where it is paid, and it narrows the race rather than closing it.** `turn.rs:244-252`: the `tokio::time::sleep(WRITER_TICK)` is taken whenever a ceiling is set and the first look was *under* it — i.e. on **every** ordinary turn of a budgeted session, delaying `running → idle` by 35 ms, not only on the rare path the sentence implies. There is also no barrier with the writer, so a >35 ms commit (slow disk, big batch) still reads a stale tally; the fallback is benign, and that is worth saying instead of "a fact rather than a coin toss". Same hunk: `self.timeout.unwrap_or_default()` (`turn.rs:190`) and `self.budget.unwrap_or_default()` sit inside arms whose guards prove the `Some`, so a `Duration::ZERO`/`0` can only ever appear in a log line that should not be reachable — `if let Some(limit) = …` says that honestly.
- **P3 — two parsers of one frame disagree.** `events/repository.rs:18-32` requires two **non-negative integers** (`as_u64`), while `web/src/transcript.ts:93-105` accepts any `number`. A `{"usage":{"in":-5,"out":1.5}}` renders `in -5 · out 1.5` on the screen and adds nothing to the tally — the screen shows a cost the budget never counted. *Fix:* the same integer/≥0 check in the TS `usage()`.
- **P3 — a re-export keeps a second path to one constant.** `http/sessions.rs:76` re-exports `events::TURN_STEER` (and `http/mod.rs:17` re-exports again) so `tests/steer.rs:16` can keep importing `holosmith::http::TURN_STEER`, although `lib.rs:10` already exports `events`. Judgement call: `Middle Man` on a two-hop `pub use` for one test import.

**Standards verdict:** no P1. Eight P3s, all small corrective edits; the diff follows the repo's prose conventions closely (each new claim names its ADR, its reason and its cost).

**Merge verdict: OK with notes** — the P2 (a two-turn accumulation test plus ADR-0027's overstatement) is cheap and is the only thing standing between "the six tests prove what the ADR says" and "they don't"; everything else is nits.

## Verification

**Verified by reading**

- The whole artifact (`/tmp/holosmith/reviews/PER-55-diff.txt`, 17 files, including untracked `tests/token_budget.rs`, ADR-0027, and the four web/script/doc hunks) plus the files it names.
- `turn.rs:176-210` (`run`), `:244-282` (`settled_over_budget`, `over_budget`), `:283-332` (`finished`); `ladder.rs:136-201`; `events/writer.rs:26-135` (25 ms collect window, commit-then-broadcast, batch dropped on error); `events/repository.rs:77-125`; `sessions/repository.rs:40-48, 260-300`.
- Config: `max_session_tokens: Option<u64>` with `0` refused (`config/mod.rs:560`), `idle_stop_min`/`turn_timeout_min` defaulting to 30/45 (`:229-231`) — so `case_token_budget`'s config, which omits both, boots with neither timer and the shared `config.toml` is a superset. No required key is missing from the second boot.
- Session JSON field order (`sessions/mod.rs:210-217`) matches the gate's `*'"usage_in":1200'*'"usage_out":340'*` pattern.
- `parseFrame`'s drop of a usage-less `message.end` is pre-existing (`transcript.ts:91-105, 161-166`), so the new test pins behaviour rather than changing it.
- Gate wiring: `serve_cases` order (`verify.sh:305-311`), `fetch_sse_until`'s signature and 800-line budget (`:229-247`), `start_serve`'s tag → `serve-data-budget.err` (`:1479-1506`), `stop_serve`'s `check serve/exit 0` (`:1510-1518`), `case_no_cors_header` running after the loop over saved files (`:1472-1478`).
- Test helpers exist with the signatures `tests/token_budget.rs` uses (`support::{state_in, with_stub, TempDir, PATIENCE}`); `add_usage` has exactly one production caller; `OptionalExtension` is already imported in `sessions/repository.rs`.

**Could not verify without running (for the orchestrator)**

1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — including whether `the_first_policy_to_fire_is_the_only_one_that_climbs` is stable: it needs the 100 ms poll to win against a 150 ms deadline on a 2.4 s turn, and if a scheduler stall makes both arms ready in one iteration `tokio::select!` picks at random, which would record `turn_timeout` and fail the assertion. A 50 ms margin is the whole budget; consider widening `poll`/`deadline` apart.
2. `npm test` in `web/` (the three new tests in `web/test/transcript.test.ts`).
3. `scripts/verify.sh` — the double boot, `wait_for_reason_id`, and whether `fetch_sse_until`'s needle is reached well inside its line budget (it should be: the budget boot's log is one turn).
4. Mutation checks I recommend, in order of value: `ADD_USAGE` → `SET` (I expect the suite to stay green — that is the P2); deleting the `fired.is_none()` guard on the timeout arm (must go red); `spend >= ceiling` → `>` with `CEILING = 1540` (adds the "at the limit" half of spec item 13's wording, which nothing pins today); `budgetNotice`'s `settled === 0` → `false` (must go red).
5. End-to-end proof of the no-usage sentence: `omitting_usage()` is reachable only from Rust tests, so the UI half is pinned solely by the pure-function tests — nothing exercises it against the shipped artifact, by design (plan item 6).
---

## Resolution

Every finding is answered: `fixed` names the change, `dismissed` names the document it rests on.

### Spec axis

| Finding | Severity | Resolution |
| --- | --- | --- |
| The accumulation the design rests on is not proven, and ADR-0027 says it is: no test has two usage-reporting turns, and `ADD_USAGE`'s `+` mutated to `SET` leaves the suite green | P2 | **Fixed.** `two_turns_add_up` runs two turns that both reach their `message.end` and asserts `(2400, 680)`; the `SET` mutation was re-run and turns it red (10 s timeout inside `wait_for_usage`). ADR-0027's consequences now name eight tests and list the four mutations with the test each one breaks. |
| A backend that reports usage for one message and not the next under-counts, so calling the contingency "disabled" overstates it | P3 | **Fixed** as documentation. ADR-0027 now says what is true: *the budget counts what was reported*, a partially-reporting backend under-counts, and per-message honesty is the backend's (HS-034's mapping) — the screen's sentence is the operator's warning. No code change: there is nothing at the row that distinguishes "reported nothing" from "cost nothing", which is exactly why the sentence is derived from the stream. |
| `GLOSSARY.md`'s "the one key whose absence is a setting" is falsified by the four other optional keys | P3 | **Fixed.** The **Token budget** entry is scoped to item 13's table and says the timers use `0` for the same job, which is what the `idle_stop_min` and `turn_timeout_min` entries already say. |

### Standards axis

| Finding | Severity | Resolution |
| --- | --- | --- |
| `sessions/repository.rs`: the HS-019 doc line was merged with a dangling `///` by the `spend` insertion | P3 | **Fixed** — the line is split again. |
| `verify.sh`: `wait_for_reason_id` was spliced between `wait_for_gone`'s doc paragraph and its body | P3 | **Fixed** — moved after `wait_for_gone`, with its own heading. |
| `serve_out` has no reader | P3 | **Fixed** — the variable is gone and stdout goes to `$WORK/$tag.out` inline, so the work dir still keeps it. |
| `WRITER_TICK` restates `WriterOptions::commit_interval` | P3 | **Fixed** — `writer_tick()` derives it from `events::WriterOptions::default()`, so a construction site with a different tick moves the wait with it. |
| The second look's comment understates where it is paid and claims to close a race it only narrows; `unwrap_or_default()` in arms whose guards prove `Some` | P3 | **Fixed.** The comment now says the delay is paid on **every** ordinary turn of a budgeted session (25 ms), that one tick's wait narrows rather than closes the window, and why that is deliberate (the next turn's first poll catches a late frame). The two arms use `let Some(limit) = self.timeout else { continue }` and `if let (Some(spend), Some(ceiling))`, so no log line can print `0`. |
| Two parsers of one frame disagree: Rust requires non-negative integers, the renderer accepted any `number` | P3 | **Fixed** — `count()` in `transcript.ts` (a non-negative integer) mirrors `as_u64`, and the parse test pins `-5` and `1.5` as drops. |
| `http::TURN_STEER` is a two-hop re-export for one test import | P3 | **Fixed** — `http/sessions.rs` imports `crate::events::TURN_STEER`, `http/mod.rs` no longer re-exports it, and `tests/steer.rs` imports it from `holosmith::events`. |

### Also found while fixing (not in the review)

- **A flake the gate caught, in this file's own first draft.** `two_turns_add_up` (and two older tests)
  read the row's tally immediately after the turn resolved, and the writer commits on a 25 ms tick — the
  gate run failed two times in five. `wait_for_usage` now waits for the value, which is the rule
  `RESUME.md` §6 states and this file had just broken. Six consecutive runs are green.
- **The double-climb test's window was 50 ms on each side.** It is now 100 ms (poll at 100 ms, deadline
  200 ms, first frame 300 ms), so a scheduler stall cannot make `select!`'s choice decide the outcome.
- **`spend >= ceiling` had no test for the `=`.** `a_session_that_has_spent_exactly_its_ceiling_is_over_it`
  pins spec item 13's "at", and `>` turns it red.
