# Review — PER-52 (HS-020, the cap and eviction)

- **Fixed point:** `3803abb`
- **Reviewed change:** `a3c6825` (`3803abb..HEAD` at the time of review; the fix commit follows)
- **Reviewer:** a fresh `reviewer` subagent running `/home/adamolsson/.agents/skills/code-review/SKILL.md`,
  read-only, no shell. Its claims were checked by the orchestrator; the Resolution table records what was
  fixed and what was dismissed.
- **Verdicts:** Spec — **blocked** on one P1 (the page's acceptance bullet held only on a reload, not on
  the frame path an eviction actually arrives by). Standards — OK with notes (one duplicated column
  list, one mangled doc comment, three sentences the change had made false).

This review is saved verbatim below this header. The `## Resolution` section at the end was appended
afterwards.

---

I have read the brief, the diff, the plan, ADR-0024 and the surrounding code. Here is the review.

---

# PER-52 / HS-020 — cap and eviction

**Fixed point:** `3803abb` → `a3c6825` (`3803abb..HEAD`), review of `/tmp/holosmith/reviews/PER-52-diff.txt`.
**Read for verification:** `crates/holosmith/src/http/sessions.rs` (create, `slot_for`, `evict_one`, `create_locked`, `resume`), `src/http/error.rs`, `src/sessions/{repository,state,activity}.rs`, `src/backend/{mod,stub,docker}.rs`, `src/config/mod.rs`, `crates/holosmith/tests/{eviction,sessions_api}.rs`, `tests/support/mod.rs`, `web/src/{main,transcript}.ts`, `web/test/transcript.test.ts`, `scripts/verify.sh`, `docs/build.md`, `docs/adr/0024-*.md`, `docs/stories/phase-1-session-lifecycle-stub.md` §HS-020, `GLOSSARY.md`, `PROMPT.md`, `HolosmithSpec.md` items 7–9.

## Spec

**Correct (with evidence)**

- *"Create at cap evicts the least recently active `idle` session → `stopped` with reason `evicted`, then proceeds."* The candidate is exactly one statement — `repository.rs:61-65`, `WHERE status = ?1 ORDER BY last_active_at ASC, id ASC LIMIT 1` with `Status::Idle.as_str()` supplied at `repository.rs:180-186` — so `starting`, `running`, `stopped`, `error` and `deleting` are all excluded by the `WHERE`, not by a filter the caller could forget. The row then moves through the one state function (`sessions.rs:373-378`, `Transitioner::apply(Trigger::Evict, Some(EVICTED))`), which re-reads the row inside its own lock and refuses `Illegal` if the candidate stopped being `idle` between the read and the move (`state.rs:392-420`) — so a second, racy writer cannot stop a session that is no longer idle. `Trigger::Evict` is `idle → stopped` in the table (`state.rs:206-210`), and `set_status` moves `stopped_at` with the state (`repository.rs:196-201`). The story's "least recently active" is `last_active_at` only, including for a resumed session, because the resume already touches it (`sessions.rs:731-734`, citing this story by name).
- *"… then proceeds."* The eviction is awaited before the insert, so the 202 cannot precede it; `eviction.rs:150-180` asserts the victim's row is already `stopped`/`evicted`/`stopped_at` when the second create's 202 lands, with no polling, and `live == 1`. `verify.sh:1108-1170` re-proves the row and the replayed `idle → stopped` frame against the shipped binary.
- *"If nothing is evictable … → 409 `cap_reached`."* `Slot::Full` + `evict_one == false` → `ApiError::cap_without_idle()` (`sessions.rs:316-325`), which is 409 with `CAP_REACHED` (`error.rs:278-285`). Proven for `starting` (`eviction.rs:250-280`, with a slow stub that actually holds the state) and for `running` (`eviction.rs:283-305`, driven through the one state function); the sentence naming "idle" is asserted twice (`eviction.rs:267-275`, `verify.sh:557-566`). The retry path is not collateral: `by_client_key` runs before `live_count` (`sessions.rs:414-427`), so a duplicate key never evicts anyone — the pre-existing `a_retry_of_the_session_that_filled_the_cap_still_answers_202` still holds.
- *"Eviction deletes nothing."* `evict_one` calls exactly two things: `Transitioner::apply` and `state.backend.stop` (`sessions.rs:373-403`). No `remove`, no `delete`, no `snapshot`. The test reads the *backend calls* rather than trusting the comment: `lifecycle(&stub, &first) == [Call::Stop]` with `Delete`/`Snapshot` unfiltered (`eviction.rs:130-140`, `:172`), the event log's old prefix is byte-equal after the eviction (`:181-186`), the worktree path is unchanged (`:164-167`), and the row still answers `200 stopped` (`:190-198`).
- *"`max_live_sessions` is configurable and validated as ≥ 1"* and *"the cap counts live sessions only"* are pre-existing and unchanged: `repository.rs:56` + `:151-163`, `config/mod.rs:542-549`, `tests/serve_cli.rs:345-353`, `tests/sessions_api.rs:467-479` (`stopped` takes no slot). This story depends on them instead of re-proving them, which the plan says out loud.

**Finding — P1. The list row does not say "nothing was deleted" on the live transition, which is the demo's path.**
- Where: `web/src/main.ts:278-281` (`applyStatus`), against `web/src/main.ts:199-203` (`sessionRow`) and `web/src/transcript.ts:307-311`.
- Problem: `stoppedNotice` is only consulted when a row is *built*. The one path by which a row becomes `stopped` while the operator is looking at it is a `session.status` frame, and `applyStatus` un-hides the `.retention` span but never writes it, so the row shows `stopped · evicted` beside the placeholder `—`. `render()` runs once (`main.ts:901-910`, `:959`) and `addSession` only prepends the new row (`:299-317`), so nothing re-renders the victim's row. Following the story's own demo (create A, wait `idle`, create B on the same page) the list therefore shows `—`, not "nothing was deleted, resume it to continue" — acceptance `docs/stories/phase-1-session-lifecycle-stub.md:239` ("…and the UI says so") is met only after a reload. `npm test` cannot catch it: the only web test imports `transcript.ts` (`web/test/transcript.test.ts`), and `applyStatus` is unexported in `main.ts`. The session screen never shows the sentence at all (`main.ts:863`, badge only).
- Smallest fix: make the text a pure function so both paths and the test share it — e.g. `retentionText(status, reason): string` in `transcript.ts` returning `stoppedNotice(reason) ?? "—"` — use it in `sessionRow` and in `applyStatus` (`retention.textContent = retentionText(frame.to, frame.reason)` next to the existing `hidden` line), and add one `npm test` case for the function.

**Finding — P2. The "least recently active" test's stated guarantee about `id` order does not exist; the regression it claims to rule out is caught only about half the time.**
- Where: `crates/holosmith/tests/eviction.rs:239-241` ("Creation order and id order both say *older*, so a candidate chosen by either would pick the wrong session"), claimed again in `docs/adr/0024-the-cap-and-eviction.md:80-83` ("proved in the direction that rules out 'oldest created' and 'lowest id'").
- Problem: ids are `SELECT lower(hex(randomblob(8)))` (`repository.rs:22-23`) — random, so the newer row's id sorts below the older one's about half the time. The `created_at` half of the claim is sound (ageing the newer row makes creation order pick the wrong session deterministically), but a regression to `ORDER BY id ASC` is caught only when `newer.id > older.id`. The test is not vacuous, but it is half-strength and the ADR records it as a proof.
- Smallest fix: pick the victim by id after both exist — read both ids, `age()` the one with the **larger** id (so `id ASC` necessarily picks the other), and assert on that one. Deterministic in both directions, three lines, and the comment then tells the truth.

**Finding — P2. `case_create_cap_reached`'s new precondition is a race with the shipped stub's provisioning pace.**
- Where: `scripts/verify.sh:534-547`.
- Problem: the case now *requires* `$created_id` to still be `starting`, but that row was created two cases earlier (`scripts/verify.sh:276-280`: `case_create_session`, `case_create_is_idempotent`, `case_create_bad_project`, then this one) and the artifact's stub settles a session in about 200 ms × 4 steps ≈ 800 ms (`backend/stub.rs:36`, `DEFAULT_STEP_DELAY`). Two `/dev/tcp` round trips are normally faster, so it passes today, but the margin is a machine-speed coincidence, and when it is lost the gate goes red on a run whose product behaviour is correct. (The comment at `:531-533` shows the author saw the race and chose "fail loudly" over "make it deterministic".)
- Smallest fix: don't guess the state — branch on the fetched row. `starting` → the `409 cap_reached` assertions as today; `idle` → `202` plus the victim's `stopped`/`evicted` row (the same assertions the next case makes). Both answers are correct and the case stops depending on the host's speed. Alternatively poll to `idle` and `POST /prompt` first, so the live session is `running` (never evictable) — but that mutates the stream `case_provisioning_steps` reads, so the branch is the cheaper fix.

**Finding — P3. The create's 202 now waits for the victim's container to stop.**
- Where: `crates/holosmith/src/http/sessions.rs:399-406` (`state.backend.stop(&candidate).await` inside `evict_one`, which is inside the request's `create`).
- Problem: spec item 7 (`HolosmithSpec.md:127-129`) exists so an answer never waits for container work, and this repository's precedent is to push backend work off the request's task (`Ladder::spawn`, `Turner::spawn`, `Provisioner::spawn`). Only the *row move* needs to be inside the answer; the stop does not. Not observable today — the stub's `stop` is immediate (`backend/stub.rs:393-398`) and `DockerBackend::stop` returns `Unsupported` immediately (`backend/docker.rs:67-69`) — but HS-024's `docker stop -t 10` will put up to ten seconds between the tap and the 202, which is the latency item 7 removed.
- Smallest fix (or record it): either spawn the stop (then `eviction.rs` needs `wait_until` around the `[Call::Stop]` assertion), or add one sentence to ADR-0024 saying the stop is awaited on purpose. Not a merge blocker; report-only.

## Standards

**Correct (with evidence)**

- Vocabulary is the glossary's, and it was extended rather than reinvented: **Evict** now carries the row's reason and the machine names (`GLOSSARY.md:160-163`), following the established "Its machine names are …" pattern (`Turn runner`, `Transition`, `Delete flow`); `EVICTED` is the third reason word beside `forced_abort` and `deleted`, as ADR-0024 argues; no second spelling of "cap", "live session", "stop" appears in the diff.
- The architectural guards hold and are argued in the diff itself: the eviction does **not** enter the create's one lock scope (`sessions.rs:299-312`; ADR-0012), it does **not** write `status` directly (`state.rs:196-201` says `set_status` has one production caller, and the source-level guard test still holds — `repository.rs:196-201`, `:366-380`), and the row moves before the container (`sessions.rs:399`, ADR-0020).
- `docs/build.md`'s "every later story adds at least one `case_*` and one line in `serve_cases`" is honoured (`verify.sh:296`, `:1108-1170`), and the shared `wait_for_idle` was split instead of copied (`verify.sh:962-983`).
- Per-file test helpers (`post_create`, `assert_envelope`) duplicate by convention, not by accident — eight test files already do it (`resume.rs`, `turns.rs`, `abort.rs`, `steer.rs`, `delete.rs`, `provisioning.rs`, `sessions_api.rs`), so `tests/eviction.rs` matches the house style rather than breaking it.
- ADR-0024 is registered where the repo registers ADRs (`docs/stories/README.md:117`, count twenty-three → twenty-four, `:142`), and its factual claims about the tests check out against the code (two web cases, `npm test` is 20).

**Finding — P2. `OLDEST_IDLE` re-spells `COLUMNS` and bypasses `select()`, which exists to forbid exactly that. Duplicated Code.**
- Where: `crates/holosmith/src/sessions/repository.rs:61-65`.
- Evidence: the file's own helper says `/// SELECT <every column> FROM sessions <filter>, so the readers cannot drift from COLUMNS.` (`repository.rs:288-291`) and every other reader goes through it (`one()` at `:274-286`, `list()` at `:132-137`). The new const hand-copies the fifteen columns from `repository.rs:17-18`. If a migration adds a column that `row_to_session` (`:307-329`) starts reading, `oldest_idle` fails at runtime with a 500 (`internal("oldest_idle")`) while every other read works — a drift the helper was written to make impossible, on the create path.
- Smallest fix: delete the const and use the existing reader — `self.one("oldest_idle", "WHERE status = ?1 ORDER BY last_active_at ASC, id ASC LIMIT 1", &[&Status::Idle.as_str()])`.

**Finding — P2. A doc comment was mangled in a file this story has no business editing.**
- Where: `crates/holosmith/src/http/sessions.rs:68`.
- Evidence: the hunk replaces `/// The event a steer is recorded as (HS-017, ADR-0021).` + `///` with `/// The event a steer is recorded as (HS-017, ADR-0021).///`, which renders as `…ADR-0021).///` followed by an unrelated sentence. The subject is HS-017's steer; HS-020 touches nothing here. `PROMPT.md:64` ("It must not touch unrelated files, and must not 'fix' pre-existing issues it happens to notice") is the documented rule this breaks; the damage is cosmetic but it is committed to `main`.
- Smallest fix: restore the two lines (`…ADR-0021).` / `///`).

**Finding — P3. Three sentences this diff made false were left standing.**
- `crates/holosmith/src/http/error.rs:262` — "What a create at the cap answers until HS-020 lets it evict." The create no longer calls it at all (`cap_without_idle` does); the only production caller left is the resume (`http/sessions.rs:720`). Smallest fix: "What a resume at the cap answers (ADR-0022, ADR-0024)."
- `crates/holosmith/src/http/sessions.rs:702-704` — "The bullet allows either branch — evicting the least recently active idle session is HS-020's — so this answers 409." HS-020 has landed and deliberately did *not* give the resume the eviction; a reader can no longer tell whether the current 409 is finished or pending. Smallest fix: name the outcome ("HS-020 gave the eviction to the create; the resume keeps the refusal, ADR-0024").
- `docs/build.md:142` — "the round trip itself cannot be driven here because nothing in the API produces `stopped` yet". This diff is the change that makes the create produce `stopped`, and the paragraph was edited three lines later to describe it. Smallest fix: replace the clause with the eviction as the producer.

**Merge verdict: BLOCK** on the P1 (the acceptance bullet "the UI says so" is unmet on the demo's own path; the fix is a handful of lines in `main.ts`/`transcript.ts` plus one `npm test` case). The two P2s in the test/gate layer (the id-order claim, the `verify.sh` precondition race) should be fixed in the same pass; the rest are report-only.

## Verification

**Verified by reading (code + test text traced against the acceptance lines):**

| Acceptance bullet | How it is proved |
|---|---|
| Cap counts `starting`/`idle`/`running`; `stopped`/`error`/`deleting` do not | `repository.rs:56` (`status IN (?1,?2,?3)`), `:151-163`; `tests/sessions_api.rs:467-479` (`stopped` takes no slot); `tests/eviction.rs:308-338` (`error` takes no slot). `deleting` is excluded by the same `WHERE` but has no dedicated test. |
| Create at cap evicts the least recently active `idle` session → `stopped`/`evicted`, then proceeds | `sessions.rs:306-330`, `:342-408`, `:412-427`; `state.rs:206-210`; `eviction.rs:142-198`; `verify.sh:1108-1170`. Order by `last_active_at ASC, id ASC` is the code (`repository.rs:61-65`); the *id* half of that tie-break is only half-proved (P2 above). |
| Nothing evictable → 409 `cap_reached` | `sessions.rs:316-325`; `error.rs:278-285`; `eviction.rs:250-280` (`starting`), `:283-305` (`running`); `verify.sh:534-566`. |
| Eviction deletes nothing — history, worktree, events, images — and the UI says so | Code deletes nothing (`sessions.rs:399-406`); `eviction.rs:172` (`[Call::Stop]`, `Delete`/`Snapshot` unfiltered), `:181-186` (log prefix unchanged), `:164-167` (worktree), `:190-198` (row still `200`). **UI: partial** — `transcript.ts:307-311` + `main.ts:199-203` say it on render only; the live path (P1) does not. |
| `max_live_sessions` configurable and ≥ 1 | `config/mod.rs:196`, `:227`, `:542-549`; `tests/serve_cli.rs:345-353`; unchanged by this diff. |

**Could not verify without running anything (and why):**

- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test`, `npm test`, `scripts/verify.sh` — the supervisor must run them; I have no shell. That includes the new `tests/eviction.rs` (five tests, several of them timing-dependent: `PATIENCE` = 10 s against the default writer commit interval, and `Served::start(…, GRACE)` = 100 ms) and the new `case_create_at_cap_evicts`, whose "the earlier session was deleted, so the slot is free" assumption (`verify.sh:1108-1113`) depends on the run's state at that point rather than on an assertion of its own.
- Whether `case_create_cap_reached`'s `starting` precondition actually holds on the run's pace — that is a wall-clock question (P2 above), not a reading question.
- Anything about the browser: there is no DOM test, so the P1 and the notice's styling (`retention` span reuse) can only be seen by loading the page. `npm test` covers `stoppedNotice` the pure function, not the two call sites.
- *"images survive"*: neither backend can destroy or keep an image today — `DockerBackend::stop` returns `Unsupported` (`backend/docker.rs:67-69`) and the stub records a call — so spec item 9's "Eviction … stop the container" is **not** proved by this diff, only its stub-level `Call::Stop` shadow. ADR-0024 says HS-024 owns that proof; that is recorded, but it means the bullet is true of the current tree vacuously.
- The Docker path of an evicting create: with `backend = "docker"` every session is `error` (nothing provisions), so an eviction of an `idle` row cannot occur there and the "backend stop failed → log and proceed" branch is exercised by no test.
## Resolution

One blocker and six notes. All six are fixed; two of them changed behaviour rather than wording, and the
P1 was found exactly where the story's own acceptance bullet lives.

| Finding | Status | How / why |
| --- | --- | --- |
| **P1** `stoppedNotice` was consulted only where a row is *built*, so the frame path — the only way a row becomes stopped while the operator is watching, and therefore the eviction's own path — showed `stopped · evicted` beside a bare `—` | **fixed, and proved in a browser** | `web/src/transcript.ts` gained `stoppedDetail(state, reason)` (the notice, or `—` for everything else) and `main.ts` now calls it from **both** `sessionRow` and `applyStatus`, with `NO_DETAIL` shared so the two paths cannot drift. `web/test/transcript.test.ts` grows a case that pins the two call sites to one function, and the demo's Chromium half now drives the live path: it watches a specific row by id turn `stopped` after a create with **no reload** and asserts the sentence. The first run of that check passed vacuously over an already-stopped row; the check was rewritten to key on the id, and on the stale bundle it then failed — which is the evidence that it is a real check. |
| **P2** the "least recently active" test claimed to rule out "lowest id" as well, but ids are random, so that half held only about half the time | **fixed** | The claim was split into two deterministic tests: ageing the *later-created* session rules out "oldest created" by construction, and a new `two_idle_sessions_of_the_same_age_are_ordered_by_id` ages two rows to the same second and asserts the smaller id goes. Proved by mutation: `id ASC` → `id DESC` fails that test and only that test. ADR-0024's consequence list says exactly what each proves now. |
| **P2** `case_create_cap_reached` required a session created two cases earlier to still be provisioning — a bet on the host's speed | **fixed by moving it, not by branching** | The refusal case is now `case_create_at_cap_refused`, last in the run, and it owns what it depends on: it creates the session that holds the slot (evicting the previous case's idle one), asserts that row is `starting`, and then asks for one more. One round trip against a 200 ms first step, and nothing after it depends on what it leaves. The eviction case ends by waiting for its own session to reach `idle`, so the refusal case starts from a known state rather than a hoped-for one. |
| **P2** `OLDEST_IDLE` re-spelled the fifteen columns instead of using `select()`, the helper written to make that drift impossible | **fixed** | The constant is now just the filter (`WHERE status = ?1 ORDER BY last_active_at ASC, id ASC LIMIT 1`) and `oldest_idle` goes through the repository's own `one()` — so a column a later migration adds is read here too, and a failure names the operation. |
| **P2** a doc comment (HS-017's `TURN_STEER`) was mangled into `…ADR-0021).///` by an edit of this change, in a file the story has no business touching | **fixed** | Restored to the two lines it was. |
| **P3** the create's 202 waited for the victim's `backend.stop`, which HS-024's real `docker stop` would turn into a ten-second stall | **fixed (behaviour)** | The stop is spawned, not awaited: the row already moved (which is what frees the slot), so the answer keeps spec item 7's "never wait for container work". ADR-0024 now argues the choice instead of leaving it to be rediscovered, and `tests/eviction.rs` waits for the recorded `Stop` instead of assuming it landed before the 202. |
| **P3** three sentences the change made false were left standing: `cap_reached`'s "until HS-020 lets it evict", the resume's "the bullet allows either branch — … is HS-020's", and `docs/build.md`'s "nothing in the API produces `stopped` yet" | **fixed** | Each now says what is true: `cap_reached` is the resume's answer, the resume keeps the refusal because HS-020 gave the eviction to the create (ADR-0024), and the build doc names the eviction as the producer of `stopped` and explains why the resume round trip is still `tests/resume.rs`'s. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (213 lib
tests, 21 integration binaries — six in `tests/eviction.rs`), `cd web && npm test` (21) and
`cd web && npx tsc --noEmit` are green; `scripts/verify.sh` passes with 36 cases and 169 assertions
against the rebuilt artifact; and the demo (cap at one, the victim's transcript, the refusal, and the
Sessions screen in Chromium on both the drawn and the live path) was run against
`dist/x86_64-unknown-linux-musl/holosmith`.

Two things the reviewer could not verify are still not verified here, and are recorded rather than
implied: "images survive" and the container really stopping are HS-024's (the stub records a call and
today's Docker backend refuses everything), and the "backend stop failed → log and proceed" branch has no
test because no backend in this build can fail a stop.
