# Review — PER-53 (HS-021, idle stop)

- **Issue:** PER-53, story HS-021 — "Idle stop", from `docs/stories/phase-1-session-lifecycle-stub.md`.
- **Fixed point:** `24056c3` (the commit before this story's work). Diff artifact:
  `/tmp/holosmith/reviews/PER-53-diff.txt`.
- **Spec sources:** `HolosmithSpec.md` items 13, 15 and 16; the HS-021 slice; the Linear body; the plan in
  `/tmp/holosmith/plans/PER-53-linear.md`; and this change's own ADR-0025.
- **Standards sources:** `AGENTS.md`, `GLOSSARY.md`, `docs/build.md`, `docs/adr/0001…0025`, and the sibling
  runners (`deleter.rs`, `ladder.rs`, `turn.rs`, `provisioner.rs`, `http/sessions.rs`'s `evict_one`).
- **Verdict:** spec axis clean; standards axis five P2s and three nits, no P1 — all addressed in the
  Resolution below.

## Review

**Target:** PER-53 / HS-021 — the change saved at `/tmp/holosmith/reviews/PER-53-diff.txt` (fixed point 24056c3). Read against `HolosmithSpec.md` items 13/15/16, `docs/stories/phase-1-session-lifecycle-stub.md` HS-021, `/tmp/holosmith/plans/PER-53-linear.md`, `docs/adr/0025-the-idle-stop-sweeper.md`.

**Correct** (with evidence):
- The candidate rule has one spelling in the right place: `IDLE_BEFORE` (`sessions/repository.rs:67-68`) carries `status = ?1 AND last_active_at < ?2`, and the table carries the same invariant (`sessions/state.rs:216-220`, `idle + IdleStop -> stopped` only). No second `if status == Idle` filter in Rust.
- The row-before-container order and the spawned best-effort stop match the eviction precedent exactly, including the `Err(StateError::Unreported(_)) => {}` arm and the "refusal is an `info` line" call (`sweeper.rs:129-171` vs `http/sessions.rs:370-390`).
- Freeing the slot is real and tested at the level the story asks: `live(&state) == 0` after a sweep (`tests/idle_stop.rs:180`) and the gate's second create being accepted at cap 1 with the old row still `idle_stop` (`verify.sh:1193-1215`).
- `stopped_at` moves in the same write because it is `set_status`'s job, not the sweeper's (`repository.rs:207-212`), so item 15 is satisfied without a second writer.
- The `list` → `many` extraction is behaviour-preserving (`repository.rs:135-163, 293-311`): same `select()`, same `row_to_session`, same short-circuit on the first repository error.
- `serve` wiring: `AppState::sweeper()` is the only reader of the key that decides the policy (`http/app.rs:126-147`), `serve` spawns before the router and aborts the handle after `serve::run` (`commands/serve.rs:140-171`), and both facts are `debug`, so the eight byte-pinned banner lines are untouched (`tests/serve_cli.rs:202`; banner is exactly eight `eprintln!`s).
- The web change reuses the one function both render paths call (`transcript.ts:324-329`, `main.ts:201, 283`), and the test that guards it also asserts it is *not* the cap's sentence (`web/test/transcript.test.ts:308-318`).

**Fixed:** none — this review is read-only.

---

## Standards

**S1 · P2 · a doc comment the code no longer supports**
`web/src/transcript.ts:299-306`

> `// The one word here is the one the machine writes: 'evicted' is a create stopping the least recently`
> `// active idle session to take its slot (HS-020) ... Every other reason is the`
> `// caller's to show as it is`

The function below it now returns **two** sentences (`evicted`, `idle_stop`, lines 308-313). The diff updated the *test's* copy of this claim ("one word wide" → "two words wide", `transcript.test.ts:321`) but left the production comment stale, and it now says the opposite of the code: an `idle_stop` row is neither "the one word" nor a reason left to the caller. Why it matters: this repo's comments are the contract (every sibling in `transcript.ts` states a rule), and a reader here will conclude `idle_stop` has no sentence.
Smallest fix: rewrite those six lines to name both words and keep the shared promise ("nothing was deleted, resume it") as the reason they may share a function.

**S2 · P2 · the plan's and ADR's "the tests read the log" claim is not met by `cargo test`**
Plan `/tmp/holosmith/plans/PER-53-linear.md:39`:

> ``one log line per stop — `tests/idle_stop.rs` reads the log; `scripts/verify.sh` reads `serve.err`.``

`docs/adr/0025-the-idle-stop-sweeper.md:104`:

> ``... the backend asked to `Stop` and nothing else, and the transition in the log;``

`tests/idle_stop.rs:193` is the closest it gets:

> `// The story's fourth bullet: the transition is in the log, with the reason a client reads.`

and what follows reads the **event log from SQLite**, not stderr — no test in `crates/holosmith/tests` installs a subscriber or captures tracing output (repo-wide read: only `tests/serve_cli.rs:46` / `tests/db_migrations.rs:391` touch a `serve.err`, as a child process's stderr file). So deleting the `tracing::info!` at `sweeper.rs:150-158` leaves `cargo test` entirely green; only the gate's `grep` (`verify.sh:1185`) would catch it. The behaviour *is* covered end-to-end, so this is a claim-accuracy problem, not a hole in the story.
Smallest fix: either add the capture (a `tracing_subscriber` test writer) or state in the ADR that the gate is the only witness to the per-stop line.

**S3 · P2 · a test whose claim is wider than what it proves**
`tests/idle_stop.rs:338-360`

> `// `sweeper()` is the one place the key is read, so its unit is proved here rather than in a demo:`
> `// a bound of two minutes leaves ninety seconds of quiet alone and stops three hundred.`

Two points (90 s inside, 300 s outside) bracket a 120 s bound only to `(90 s, 300 s]`. A mutation of the constant to 90 or 180 s per minute passes **all seven** tests (90 stays inside 180, 300 stays outside; the 1-minute tests use 30 s and 3600 s, which also survive). So the test proves "minutes, not seconds" and cannot distinguish "one minute" from "one-and-a-half". The ADR records a mutation run ("deleting the comparison fails three of the seven", `0025:106-107`) — deleting is all it was proved against.
Smallest fix: add a third session aged just past the bound (e.g. `age(…, 130)` expecting `Stopped`) in the same test; that pins the bound to `(90 s, 130 s]` and fails a ×1.5.

**S4 · P2 · the query's predicate — the rule the code says is "one spelling" — is observed by no test**
`sweeper.rs:10-12`

> `//! - **Only `idle` is a candidate, and that is the query.** ... The repository's`
> `//!   `idle_before` carries the predicate, so there is no second place for the rule to be forgotten.`

and `sessions/repository.rs:155-160` ("The `status = 'idle'` predicate is the whole of the story's second bullet"). Delete `status = ?1` from `IDLE_BEFORE` (`repository.rs:67-68`) and every test still passes: `a_running_error_or_deleting_session_is_not_a_candidate` (`idle_stop.rs:274-317`) and `a_starting_session_is_not_a_candidate` (`:319-336`) assert only the *outcome*, and the outcome is unchanged because the table has no `running/starting/error/deleting + IdleStop` row, so `apply` returns `Illegal` and no `Call::Stop` is recorded. `oldest_idle` has the same coverage question, but the cap's cases at least observe the victim's identity.
ADR-0025 §"One apply per candidate" (`:60-65`) owns the decision to keep *both* guards; it does not supply the missing observation, so "the rule has one spelling" is currently a claim nothing checks.
Smallest fix: three lines in `tests/idle_stop.rs` — `drive` one row to `running`, `age` it past the cutoff alongside an idle one, and assert `database.sessions().idle_before(unix_now() - 60)` returns only the idle id.

**S5 · P2 · unchecked arithmetic on a config number, against the repo's own stated rule**
`crates/holosmith/src/http/app.rs:144`

> `std::time::Duration::from_secs(idle_min * 60),`

`idle_stop_min` is a bare `u64` with no ceiling (`config/mod.rs:478-...`, `semantic_problems` checks `max_live_sessions`, `stopped_ttl_days` and `max_session_tokens` and has no check for this key — which the ADR itself states at `0025:39`). The crate already wrote the rule down for exactly this situation, `http/sessions.rs:312-316`:

> ``// `saturating_add`, not `+ 1`: the bound is a safety net, and an arithmetic panic in a request``
> `// path for a number nobody can reach is still a panic waiting for a 32-bit build.`

A debug build panics at `serve` startup for a `u64` this loader accepts; a release build wraps, and a wrapped small/zero bound turns the sweeper into "stop every idle session on the first tick". This is the only `* 60` in `src/`.
Smallest fix: `idle_min.saturating_mul(60)` (or `Duration::from_secs(idle_min).saturating_mul(60)`).

**S6 · nit · `stop()`'s doc says the opposite of its body**
`sweeper.rs:127`

> `/// Stop one candidate that the sweep has already chosen, and say nothing when the table refuses.`

The code logs an `info!` on exactly that path (`sweeper.rs:138-148`, `"the idle stop did not happen"`), and ADR-0025 (`:70-72`) says an `info` line is deliberate. "Say nothing" reads as "no line at all".
Smallest fix: "…and log an `info` line, not an error, when the table refuses."

**S7 · nit · the ordering clause has two spellings**
`sessions/repository.rs:61` and `:67-68`

> `const OLDEST_IDLE: &str = "WHERE status = ?1 ORDER BY last_active_at ASC, id ASC LIMIT 1";`
> `/// The order is `oldest_idle`'s, so a sweep stops the quietest session first`
> `const IDLE_BEFORE: &str = "WHERE status = ?1 AND last_active_at < ?2 ORDER BY last_active_at ASC, id ASC";`

The doc claims the order *is* `oldest_idle`'s, but it is re-typed; change one and the claim becomes false silently. This file's own doctrine is that a filter is a fragment so a column added by a migration "cannot be missed here" (`:60-61`) — the same argument applies to the order.
Smallest fix: one `const LRU: &str = "ORDER BY last_active_at ASC, id ASC";` both formats use. (Judgement call, not a violation.)

**S8 · nit · the gate's comment names an eviction that cannot happen**
`scripts/verify.sh:1131-1134`

> `# "Frees a slot" is asserted *without* a second policy in the way: the cap is one, so the next create`
> `# either finds the slot free or evicts the stopped session and rewrites its reason. It is accepted, and`
> `# the stopped session still says `idle_stop` — which is the whole proof.`

A `stopped` row can never be evicted: `oldest_idle` filters `status = ?1` with `Status::Idle` (`repository.rs:61`, `:191-196`), and a stopped row is not live (`LIVE_COUNT`, `:58`), so the create in that branch simply inserts. The fallback the sentence describes is unreachable as written. The *real* honesty in this case is fine — if the sweep had not run, the row would still be `idle`, the create would evict it, and the `idle_stop` re-read at `:1212-1215` would fail — so only the wording is wrong.
Smallest fix: "…the next create finds the slot free because a stopped row holds none; if the sweep had not run, that create would have evicted the row as `evicted` and the re-read below would say so."

**S9 · P2, report-only · the gate now runs a live policy against every case, and nothing says so**
`scripts/verify.sh:1404`

> `printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\nidle_stop_min = 1\n' "$owner" >"$WORK/config.toml"`

The bound is process-wide, and the sweeper's first pass is immediate with a 5 s tick (`sweeper.rs:93-98`, `:52`). So from `start_serve` onwards, all 37 cases run with a background task that transitions rows, while ~20 of them share the single session created in `case_create_session` and deleted in `case_delete_session`. Any stretch in which `created_id` sits `idle` for ≥ ~65 s now stops it underneath the case that follows, and the failure would blame the wrong subject (`case_session_detail` / `case_steer` / `case_prompt_turn` would see `stopped`). Nothing in the gate's comments or `docs/build.md:149-153` records that assumption. I could not show today's case set crosses 60 s of continuous idleness, so this is a determinism risk rather than a defect — but it is new with this diff and it is invisible.
Smallest fix (docs only, since ADR-0025 owns the "no shorter lever" decision): say in `case_idle_stop`'s comment and `docs/build.md` that `idle_stop_min = 1` is process-wide, so no case may hold a session idle for a minute.

---

## Spec

**No spec findings.** Every acceptance bullet is implemented and observable:

| Story / spec line | Where it is satisfied |
|---|---|
| "moves `idle` sessions whose `last_active_at` is older than `idle_stop_min` to `stopped` with reason `idle_stop`" | `repository.rs:67-68` + `sweeper.rs:105-171`; `IDLE_STOP` (`:45`); `idle_stop.rs:149-209`; `verify.sh:1155-1178` |
| "never touches `running`, `starting`, `error` or `deleting`" | query predicate + table (`state.rs:216-220`); `idle_stop.rs:274-336` |
| "Stopping frees a cap slot immediately" | row written before the spawned `stop` (`sweeper.rs:150-171`); `idle_stop.rs:180`; `verify.sh:1193-1215` |
| "a `session.status` event with the reason" | `Transitioner::apply` (`state.rs:361-408`); asserted `from/to/step/reason` at `idle_stop.rs:194-209`; replayed in the gate (`verify.sh:1180-1192`) |
| "a log line per stop" | `sweeper.rs:150-158` (`info`, default filter `info` — `logging.rs:16`); gate `grep` (`verify.sh:1185`) |
| item 13 default 30, item 15 `stopped_at` | `config/mod.rs:230` (`idle_stop.rs:373`); `set_status` (`repository.rs:207-212`) |

Deliberate deviations I checked and am **not** reporting because ADR-0025 already owns them: `0` as the off switch (`:37-52`), the tick as a constant (`:21-35`), every candidate per pass rather than one (`:56-58`), the distinct page sentence instead of the plan's "reuse the eviction's sentence" (`:74-84`), and `debug` lines instead of a ninth banner line (`:46-51`).

---

## Verified by reading

| Check | Verified in |
|---|---|
| Query predicate is `idle`-only and time-bounded, one comparison | `sessions/repository.rs:67-68, 158-163` |
| Table refuses `IdleStop` from every non-idle state | `sessions/state.rs:216-220` (`from: OneOf(&[Status::Idle])`), `lookup` at `:243-247` |
| Row before container, stop spawned and best-effort | `sweeper.rs:129-171`; precedent `http/sessions.rs:370-395` |
| Refusal is `info`, `Unreported` still stops the container | `sweeper.rs:136-148`; precedent `http/sessions.rs:376-390` |
| Slot freed = live count drops; `stopped_at` set | `repository.rs:58, 207-212`; `live_count` `:166-181` |
| `idle_stop_min = 0` spawns nothing (one decision site) | `http/app.rs:135-146`; `commands/serve.rs:144-161`; `idle_stop.rs:363-377` |
| Banner untouched by the two `debug!`s; default filter is `info` | `commands/serve.rs:200-224` (8 `eprintln!`), `logging.rs:16, 24-34` |
| `list` refactor preserves behaviour | `repository.rs:135-163, 293-311` vs pre-diff (same `select`, same short-circuit) |
| `many` is the only collection statement (no hand-copied column list) | `repository.rs:293-311`, `select` at `:313-315` |
| Tests are deterministic (age, no waits; slow stub = 60 s/step) | `idle_stop.rs:99-105`, `tests/support/mod.rs:103-120` |
| Gate case: row, reason, transcript, log line, freed slot; then cleanup | `verify.sh:1126-1219`, helpers `:1050-1087` |
| Gate ordering: after `case_delete_session` (table empty, asserted at `:1112-1120`) and before the cap cases | `verify.sh:292-294` |
| `docs/build.md`, `GLOSSARY.md`, `docs/stories/README.md` (24→25) consistent with code | `build.md:95-100, 148-153`; `GLOSSARY.md:164-179`; `README.md:117, 143` |
| `npm test` count claim ("22") and test-file count (7) | 22 `^test(` in `web/test/transcript.test.ts` (only test file under `web/test`); 7 `#[tokio::test]` in `tests/idle_stop.rs` |

## Could not verify without running

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `npm test`, and `scripts/verify.sh` (all five gate phases) — no shell here; every "this test would/wouldn't fail" statement above (S3, S4) is reasoned from the source, not observed.
- That tracing's default `fmt` layer renders the sweeper's line exactly as the gate greps it (`holosmith::sweeper: the session sat idle for too long and was stopped session=<id>`). I confirmed the limiter is right (`target:` is printed by default, `%` fields render unquoted, default filter `info`) and the field order in the macro, but not the byte sequence in `serve.err`.
- Whether any existing gate case holds `created_id` idle for the ~65 s that would now trigger a stop (S9). Needs a timed gate run; the `case_abort` ladder (default `abort_grace = 10s`) is the longest stretch I can identify, which is under the bound.
- That the release-profile wrap in S5 produces a small/zero bound rather than a panic (reasoned from Rust arithmetic semantics; `overflow-checks` for the dist profile not inspected).
- The claim in ADR-0025:9-11 ("three reviews in a row have pointed at a test that had to `wait_until`…") — unverifiable from this tree alone; not treated as a finding.

**Verdict:** Spec axis clean; Standards axis has five P2s and three nits and no P1 — merge after fixing S1 (stale `transcript.ts` rule), S2 (plan/ADR claim about the log line) and S3 (widen-proof of the bound), with S4/S5 tracked as follow-ups.

## Resolution

Every finding below was re-checked against the code before it was triaged; the two claims the reviewer
flagged as *unproved* (S3, S4) were settled by mutation, and the results are in the third column.

| # | Sev | Finding | Resolution |
| --- | --- | --- | --- |
| S1 | P2 | `transcript.ts`'s rule comment still said "the one word here" after `stoppedNotice` grew a second one | **Fixed.** The comment now names both machine-written reasons (`evicted`, `idle_stop`), says what the two sentences share (the promise, not the cause), and keeps "every other reason is the caller's". `stoppedDetail`'s comment names both arriving paths too. |
| S2 | P2 | The plan and ADR-0025 claimed `tests/idle_stop.rs` "reads the log"; no test in `tests/` captures tracing output, so only the gate sees the per-stop line | **Fixed, in the ADR (the durable record) and in the test's own comment.** ADR-0025's Consequences now say the stderr line is the gate's witness and why no test-side capture was added; `tests/idle_stop.rs` says "the *event* log" where it means SQLite. The plan under `/tmp` is ephemeral and left as written. |
| S3 | P2 | `the_bound_is_the_config_key_in_minutes` bracketed a 120 s bound with 90 s and 300 s, so a ×1.5 mutation passed all seven tests | **Fixed.** A third session aged 130 s must stop, which pins the bound to `(90s, 130s]`. Proved by mutation: `idle_min.saturating_mul(90)` fails that test and nothing else. |
| S4 | P2 | Deleting `status = ?1` from the candidate query left every test green — the table refuses the move anyway, so no assertion could tell which guard fired | **Fixed.** `only_idle_rows_are_even_read_as_candidates` reads `idle_before` directly with an old `idle` row and an old `running` one, and asserts the read answers exactly the idle id. Proved by mutation: replacing the predicate with `?1 != ''` fails that test and nothing else. |
| S5 | P2 | `idle_min * 60` is unchecked arithmetic on a `u64` the config layer accepts without a ceiling; a wrapped bound would make the sweeper stop everything on its first tick | **Fixed.** `idle_min.saturating_mul(60)`, citing the create's own `saturating_add` bound as the precedent. |
| S6 | nit | `Sweeper::stop`'s doc said "say nothing when the table refuses" while the body logs `info` | **Fixed.** The doc says "log an `info` line — not an error — when the table refuses". |
| S7 | nit | `IDLE_BEFORE` re-typed `oldest_idle`'s `ORDER BY` while its doc claimed the order *is* `oldest_idle`'s | **Fixed differently from the suggestion, and for the same reason.** A `QUIETEST_FIRST` constant *and* two filter builder functions: the SQL is assembled in one place, so the order is written once, and each reader keeps its own `LIMIT`. `concat!` cannot take a constant, which is why the filters are functions rather than constants. |
| S8 | nit | The gate's comment described an eviction of a `stopped` row, which `oldest_idle` can never choose | **Fixed.** The comment now says a stopped row holds no slot so the create inserts, and states the real proof: had the sweep not run, the row would be `idle` and the create would have evicted it as `evicted`, which the re-read would show. |
| S9 | P2 (report-only) | `idle_stop_min = 1` is process-wide, so every case in the gate now runs with a sweeper and nothing recorded that constraint | **Fixed as documentation, which is what the finding asked for.** `case_idle_stop`'s comment and `docs/build.md`'s `serve_cases` section both state it as a rule for later stories: no case may leave a session `idle` for a minute. ADR-0025 records it too. |
