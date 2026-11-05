# Review — PER-66 / HS-034 (RPC mapping: usage)

Saved verbatim from a reviewer child (`~/.agents/skills/code-review/SKILL.md`, two axes) against
`c2747b0...b312bad` plus untracked files, with a `## Resolution` table appended after the fixes. Its "could not
verify without running" list is what the orchestrator then ran, mutation checks included.

## Review

Reviewed `c2747b0...b312bad` (2 commits) for PER-66 / HS-034 against the diff artifact **and** the files: `src/rpc.rs`, `tests/rpc.rs`, `tests/token_budget.rs`, the three committed fixtures, `scripts/verify.sh`, `docs/build.md`, `docs/stories/README.md`, `docs/adr/0040`, `docs/adr/0042`, `events/mod.rs`, `GLOSSARY.md`, `web/src/transcript.ts`, `web/test/transcript.test.ts`, `RESUME.md`.

### Correct (with evidence)

- **The arithmetic is right and it is checked against something outside the code.** `rpc.rs:641-642` computes `in = input + cacheRead + cacheWrite`, `out = output`. Every committed fixture agrees with pi's own `totalTokens`: text `991+640+4 = 1635` (`pi-turn-text.jsonl:12`), thinking `1492+10 = 1502` with `reasoning: 4` outside it (`pi-turn-thinking.jsonl:16`), tool `1408+256+76 = 1740` and `93+1664+5 = 1762` (`pi-turn-tool.jsonl:50,66`). The two moved expectations (`tests/rpc.rs:156-161`, `:715-718`, `src/rpc.rs:781-786`) match those numbers exactly, and `tests/rpc.rs:204-239` is a property test that **could** fail: counting only `input` gives 995 ≠ 1635, and folding `reasoning` in gives 1506 ≠ 1502.
- **`checked == 4` means what it says.** The three fixtures hold exactly four `message_end` records carrying `usage` + `totalTokens` (text 1, thinking 1, tool 2), and a record *without* `totalTokens` falls through `continue` and lowers the count, so a silent skip fails the guard rather than hollowing it out.
- **The zero rule is on the sum, and the corollary is real.** `rpc.rs:643-645`; the all-cache arm (`src/rpc.rs:824-833`, `{"cacheRead": 512}` → `{"in":512,"out":0}`) pins the case the rule exists for. A zero sum can only mean "the provider answered nothing" — pi's interim `message_start`/`message_update` zeros never reach `usage()` because `end()` is the only caller (`rpc.rs:354-356`).
- **Nothing is lost by emitting no key.** The writer adds only when the key parses (`events/repository.rs:231-237`), so a zero add was a no-op; the screen's `budgetNotice` reads the frame (`web/src/transcript.ts:376-381`), so "no usage reported" now appears where a `0 · 0` line would have. Writer and screen agree by construction, and each half is pinned (`src/rpc.rs:797-811`, `web/test/transcript.test.ts:493-512,538-542`).
- **`case_usage_survives_resume` is not the SKIP landmine this repo has hit.** The `python3` guard (`verify.sh:2337-2340`) returns **before** `stop_serve` (`:2342`), so a SKIP leaves the running boot alone; and `case_compaction` opens with its own guard-then-`stop_serve` (`:2496-2504`), so leaving `data-usage` booted is safe. The case owns its state (own config, own data dir), evicts by cap with `max_live_sessions = 1`, and its reason assertion is the right *shape* — `wait_for_stopped_id` + `"status_reason":"evicted"` (`:2385-2393`), matching `case_create_at_cap_evicts` (`:2127`) rather than the turn policies' `wait_for_reason_id`.
- **`docs/stories/README.md` is consistent:** count at `:117` reads Forty-two, ADRs `0001`–`0042` are 42 files, and the new bullet sits after ADR-0041 at `:160`, before "Gaps".
- **ADR-0042's "against the real backend" is honest, not glossed.** It names exactly what the composite test does (records → `rpc::Mapping` → writer → row → `Turn::run`) and exactly what nobody has seen ("nobody has yet seen a container's usage land in a row"), and `replay_real_turn` (`tests/token_budget.rs:446-465`) says "no agent, no container" in its own doc. It bypasses `rpc::pump` (no `turn.*` boundary pair, no tool-output file) — which the ADR's wording covers.

### Fixed

None — read-only review, no write tool.

---

## Standards

Documented standards in force: `docs/build.md` (gate rules), `GLOSSARY.md` (vocabulary authority), `RESUME.md` (house rules), the ADR amendment convention (`docs/adr/0019:67`, `docs/adr/0040:58`), plus the smell baseline. `AGENTS.md` carries only Linear scope. No `CONTRIBUTING.md`/`CODING_STANDARDS.md` exists.

**S1 — P1. ADR-0040 still states the opposite of the shipped arithmetic, and is not amended.**
`docs/adr/0040-the-mapping-layer.md:69-72`: "`usage` stays optional: emitted when pi reports **both** counts, omitted when it does not. `in` is pi's `input` and `out` its `output`; the cache counters are deliberately **not** folded in — HS-034 owns 'real token counts'…". Both clauses are now false (`rpc.rs:641-645` folds the cache counters and tolerates partial reports). `docs/adr/0040:202` ("Fold cache counters into `usage` — HS-034") is now decided and unmarked. ADR-0042 cites 0019/0040 for the payload shape and calls itself "that change's record", but never adds the back-note the repo's own convention uses (ADR-0019:67 "**Amended by ADR-0040:**"). A reader who consults the mapping's ADR gets the wrong arithmetic.
*Smallest fix:* add "**Amended by ADR-0042:**" to ADR-0040:69-72 restating `in = input + cacheRead + cacheWrite` and the zero rule, and append "(decided by ADR-0042)" to `:202`. Two lines.

**S2 — P2. `docs/build.md:144-148` splices the new case into the wrong clause, producing a false sentence and a duplicate.**
The text now reads "…`404 not_found` envelopes), **the turn timeout's quiet half** (`case_usage_survives_resume`, which prompts a session on a boot of its own, …), **and the timeout's quiet half** (`case_turn_timeout`, …)". `case_usage_survives_resume` is not the timeout's quiet half, and "the … timeout's quiet half" now appears twice. This is the same class of defect PER-93 S1/S3 recorded (a scripted insert into prose).
*Smallest fix:* make the inserted clause read "the spend across a stop and a resume (`case_usage_survives_resume`, …)" and leave "the turn timeout's quiet half" attached to `case_turn_timeout`.

**S3 — P2. The pinned `message.end` contract doc omits the zero rule.**
`crates/holosmith/src/events/mod.rs:42-48` is the code-level contract for the payload and says `usage` "is optional: a backend that cannot report it emits the frame without it (HS-023)". After this diff there are two reasons the key is absent (no report; a zero report), and the cache-fold changes what the two numbers *count*. ADR-0040's changes were reflected here (`:44-47` names ADR-0040); ADR-0042's are not.
*Smallest fix:* one clause: "…a backend that cannot report it, **or that reports a zero sum**, emits the frame without it (HS-023, ADR-0042), and `in` is the prompt's total including its cached part."

**S4 — P2. `scripts/verify.sh:2398-2403` reimplements `remove_session` inline.**
`remove_session` (`verify.sh:1896-1909`) is exactly "delete a session and wait for its row to be gone, so a case that made one of its own leaves the machine as it found it", and other own-pair cases use it (`:2052-2053`, `:2236`, `:2246`). The new case spells out the same `fetch DELETE` → `expect_status … 202` → `wait_for_gone` sequence with its own labels. Duplicated Code; the comment above it ("here it is the slot's") argues *why a delete*, not *why not the helper*.
*Smallest fix:* `remove_session "${name}_delete" "$second"` (and drop `wait_for_gone "${name}_gone"`, which `remove_session` already does).

**S5 — P3. Two naming/reuse warts in the new case.** `verify.sh:2334` declares `response`, which nothing in the function reads (it uses `answer`). `second` is the second *session id* at `:2381` and the row's *counts* at `:2412` (Mysterious Name — the same name holds two different things nine lines apart).
*Smallest fix:* drop `response`; call the counts `resumed`.

**S6 — P3. A literal newline was baked into an assertion message.** `crates/holosmith/tests/rpc.rs:159`: `"…which is\n         what the budget counts (HS-034, ADR-0042)"` — the failure text will print a newline plus nine spaces. The file's own convention, 21 lines above at `:137-138`, is a `\` continuation.
*Smallest fix:* replace `\n         ` with `\` + line break.

**S7 — P3. `GLOSSARY.md:337-343` (Usage) does not carry the zero rule** while ADR-0042 introduces it as vocabulary ("a report of nothing is not a report"). The entry stays true; it just doesn't say that a `message.end` carrying zeroes is now indistinguishable from one carrying none. Same one-clause fix as S3, if it is wanted there too.

**S8 — P3. `RESUME.md:564-565` is stale by one case and one own-boot case.** It reads "**51 cases plus a CORS sweep (434 assertions)**" and "**Eleven** cases bring their own boot"; this diff makes it 52 cases / ~447 assertions (I count 13 new assertions) and twelve own-boot cases. Per PER-46's S12 precedent this file is refreshed by the handoff commit, so it is a note for that commit rather than a defect of this one.

---

## Spec

Spec source: Linear PER-66 body + `docs/stories/phase-1b-docker-backend.md` § HS-034 (`:173-186`) + `HolosmithSpec.md` item 13 / V3.

- Acceptance 1 ("persisted to `usage_in`/`usage_out` and emitted as an event") — **met**: `tests/token_budget.rs:467-499` asserts the row's two columns are the fixture's counts (`wait_for_usage(…, REAL_IN, REAL_OUT)`, `:94-99`, exact equality), and the frames are the log's events.
- Acceptance 2 ("missing usage leaves the budget disabled and surfaces a visible warning once per session") — **met, by pre-existing machinery**; the diff adds the second path into it (a zero report now also disables and also warns) and pins the frame side (`src/rpc.rs:797-811`).
- Acceptance 3 ("exercised against the real backend, not only the stub") — **partial, and recorded** (SP2).
- Acceptance 4 ("counts survive stop and resume") — **met twice**: `tests/token_budget.rs:501-521` at the column, `scripts/verify.sh:2333-2414` on the shipped artifact.

**SP1 — P2. One third of the arithmetic cannot fail a test: `cacheWrite`.**
`src/rpc.rs:641` adds `part("cacheWrite")`, but **no fixture has a non-zero `cacheWrite`** (`pi-turn-text.jsonl:12`, `pi-turn-thinking.jsonl:16`, `pi-turn-tool.jsonl:50,66` all report `"cacheWrite":0`), and the new unit test's all-cache arm also uses `cacheWrite: 0` (`src/rpc.rs:828`). So deleting `+ part("cacheWrite")` leaves the whole suite green — including the property test that ADR-0042 calls "the *test*, not the comment" (`docs/adr/0042:33-37`). The `reasoning` and `input`-only mutations *are* caught; this one is not.
*Smallest fix:* one more arm in `a_usage_of_nothing_is_no_usage_at_all`: `{"input": 0, "cacheRead": 0, "cacheWrite": 512, "output": 0, "totalTokens": 512}` → `{"in": 512, "out": 0}`.

**SP2 — P2, report-only. Acceptance bullet 3 is met in the "records" sense only, and the story should not be closed as fully met.**
The composite test never uses the Docker/pi backend: the ceiling turn runs on `paced(200)` (`tests/token_budget.rs:471-477`), and `replay_real_turn` writes frames straight to the writer, bypassing `rpc::pump` and the container. ADR-0042 states this plainly and assigns the container half to HS-046 ("nobody has yet seen a container's usage land in a row") — which is the right call and is *recorded rather than glossed*, but it means the Linear acceptance box should be ticked with that qualifier, not bare.

**SP3 — P3. The composite test's final assertion is weaker than its sibling's.**
`tests/token_budget.rs:494-497` asserts `ended.usage_in >= REAL_IN && ended.usage_out >= REAL_OUT`, so a doubled tally passes. The equivalent assertion in `a_turn_that_spends_the_budget_ends_with_the_reason` (`:203-208`) uses exact equality for the same shape (paced 200 ms, first poll at 100 ms, abort before `message.end`), so `assert_eq!((ended.usage_in, ended.usage_out), (REAL_IN, REAL_OUT))` is safe here and is what "the tally kept the real turn's counts" means.

**SP4 — P3. `counts_survive_a_stop_and_a_resume` is close to tautological.**
`tests/token_budget.rs:501-521` drives `IdleStop → Resume → PiReady` with no runner and no backend, then asserts the column is unchanged. It can only fail if a transition writes `usage_*`, which no transition does — so it pins the claim structurally rather than empirically. Acceptable *because* the gate case (`verify.sh:2333`) is the real proof, but the unit test's comment ("neither the ladder nor the provisioner touches it") describes an argument, not an observation.

**SP5 — P3, report-only. `web/src/transcript.ts:127-143` would treat `usage: {in:0,out:0}` as reported.**
`parseFrame` accepts any two non-negative integers, so a zero payload would set `spend.usage = true` and *hide* the notice. Unreachable from this server after the zero rule, so nothing needs changing today — but the web half of ADR-0042's "one fact instead of two" rests on the producer, not on a rule the page states. `web/test/transcript.test.ts:545` pins only the missing-key case.

---

### Verified by reading

| Claim | Where | Verdict |
|---|---|---|
| `in = input + cacheRead + cacheWrite`, `out = output` | `src/rpc.rs:638-646` | ✓ |
| `in + out == totalTokens` on all 4 fixture records | `pi-turn-{text,thinking,tool}.jsonl` | ✓ 1635 / 1502 / 1740 / 1762 |
| `checked == 4` is the true count of checkable records | `tests/rpc.rs:236-239` | ✓ (1 + 1 + 2) |
| Zero sum → no `usage` key; all-cache → `{"in":512,"out":0}` | `src/rpc.rs:643-645`, `:797-833` | ✓ |
| `cacheWrite` term is exercised | — | ✗ (SP1) |
| `reasoning` folded in would fail the property test | `tests/rpc.rs:204-239` | ✓ |
| Composite test's row = the fixture's counts (3421 / 81) | `tests/token_budget.rs:439-440,467-479` | ✓ |
| Gate case SKIPs *before* `stop_serve` | `verify.sh:2337-2342` | ✓ |
| Eviction reason shape (`stopped` + `evicted`), not the turn policies' | `verify.sh:2385-2393` vs `:2127` | ✓ |
| Case leaves nothing behind / next case re-boots | `verify.sh:2333-2414`, `:2496-2504` | ✓ |
| `remove_session` exists and is the house pattern | `verify.sh:1896-1909, 2052, 2236` | ✓ (SP/S4) |
| ADR-0042's cited numbers (1492/10/4/1502; 1408/76/256/1740; 93/1664) | `docs/adr/0042:28-31`, fixtures | ✓ |
| `budgetNotice` still says the right thing; zero case collapses into the tested "no usage" case | `web/src/transcript.ts:376-381`, `web/test/transcript.test.ts:493-512,538-542,545` | ✓ |
| `docs/stories/README.md` count 42 = 42 ADR files; bullet placed contiguously | `README.md:117,160` | ✓ |
| `case_usage_survives_resume` described once and accurately in `docs/build.md` | `build.md:145,240` | ✗ (S2) |
| ADR-0040 amended for the arithmetic change | `docs/adr/0040:69-72,202` | ✗ (S1) |

### Could not verify without running (for the orchestrator)

1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — including that `every_usage_the_fixtures_report_adds_up_to_pis_own_total` compiles and that `checked == 4` holds, and that `replay_real_turn`'s `Value`/`json!` imports resolve in that binary.
2. Whether `a_real_turns_usage_becomes_the_sessions_tally_and_trips_the_ceiling` is stable: it relies on the 100 ms budget poll winning against a 2.4 s turn (`paced(200)`), the same margin the existing budget test uses — I expect green, but the margin is one scheduler stall.
3. `scripts/verify.sh` end to end — above all whether `usage_of`'s **read-only WAL connection** (`verify.sh:2314-2327`, `file:…?mode=ro`) succeeds against a live `app.db`; the other `python3` readers (`expected_stop_of`, `case_compaction`) open read-write, so this is the first `mode=ro` read in the gate. Also whether the cap-1 eviction and the subsequent resume really answer `202`/`idle` in that order, and whether the closing count is 52 cases / ~447 assertions.
4. Mutation checks, in value order: delete `+ part("cacheWrite")` (**expect green — that is SP1**); change `input == 0 && output == 0` to `input == 0` (must go red on the all-cache arm); make `usage()` return `Some` for a zero sum (must go red on `a_usage_of_nothing_is_no_usage_at_all`); swap the `>=` at `tests/token_budget.rs:494` for `==` (expect green — which is why SP3 is only P3).

---

**Summary:** Standards — 8 findings (worst: **S1 P1**, ADR-0040 states the opposite of the shipped arithmetic and is not amended). Spec — 5 findings (worst: **SP1 P2**, the `cacheWrite` term of the arithmetic cannot fail any test). The code itself is correct and the evidence is real; every finding above is documentation, test-strength, or gate hygiene.

**Merge verdict: OK with notes.** Nothing here blocks mechanically — `src/rpc.rs` does what ADR-0042 says and the fixtures prove it. Land S1 (two lines in ADR-0040), S2 (one clause in `docs/build.md`) and S3 (one clause in `events/mod.rs`) in this story's own commit rather than deferring them; S4 and SP1 are small and worth taking with them.

---

## Resolution

Every finding was checked against the code before it was acted on. The code itself came through: the reviewer
found no behavioural defect, and both P1/P2 findings were about the record and about a term no fixture reaches.

### Standards

| Finding | Verdict | What changed |
| --- | --- | --- |
| **S1 · P1** — ADR-0040 still says the cache counters are *not* folded in, and is not amended | **Fixed** | ADR-0040 carries an "**Amended by ADR-0042:**" note under the `usage` bullet restating `in = input + cacheRead + cacheWrite`, `out = output` and the zero rule, and its follow-up line now reads "decided by ADR-0042". |
| **S2 · P2** — `docs/build.md` spliced the new case under "the turn timeout's quiet half", so two clauses shared one name | **Fixed** | The clause reads "the spend across a stop and a resume" and `case_turn_timeout` keeps its own sentence. |
| **S3 · P2** — the code-level contract in `events/mod.rs` does not carry the zero rule or what `in` counts | **Fixed** | One clause: `usage` is absent when a backend cannot report it "**or reports a sum of zero**, which is the same thing said twice (HS-023, ADR-0042)", and `in` is the prompt including its cached part. |
| **S4 · P2** — the new gate case reimplements `remove_session` inline | **Fixed** | It calls `remove_session "${name}_delete" "$second"`, which is the house way and already waits for the row. |
| **S5 · P3** — an unused `response` local, and `second` holding two different things | **Fixed** | `response` dropped; the counts live in `counts`, and `second` is only ever the second session's id. |
| **S6 · P3** — a literal newline baked into an assertion message | **Fixed** | A `\` continuation, like the neighbouring assertion. |
| **S7 · P3** — `GLOSSARY.md`'s Usage entry does not carry the zero rule | **Acknowledged** | The entry stays true and the rule now lives where a producer and a renderer both read it: `events/mod.rs` (the contract), `rpc.rs` and `transcript.ts`. A glossary entry that restated the arithmetic would be a third spelling of a number's meaning. |
| **S8 · P3** — `RESUME.md` is stale (case count, own-boot count) | **Fixed** | Refreshed in the handoff commit, which is this file's own precedent. |

### Spec

| Finding | Verdict | What changed |
| --- | --- | --- |
| **SP1 · P2** — `cacheWrite` cannot fail any test (no fixture has a non-zero one) | **Fixed, mutation-proved** | `a_usage_of_nothing_is_no_usage_at_all` gained a cache-write-only arm. Deleting `+ part("cacheWrite")` now fails with "a prompt the provider had to write to its cache still cost something"; restored, the suite is green. |
| **SP2 · P2** — acceptance 3 is met in the "records" sense only | **Acknowledged, and reflected in the ship note** | Ticked with the qualifier ADR-0042 gives it: real records through the production mapping and a real runner over the row; the container half is HS-046's, because a container cannot complete a turn until the key proxy exists (HS-039). |
| **SP3 · P3** — the composite test's final assertion is `>=`, so a doubled tally passes | **Fixed** | Exact equality, with the reason recorded: the budgeted turn is aborted at its first poll, before its own `message.end`. |
| **SP4 · P3** — `counts_survive_a_stop_and_a_resume` is close to tautological | **Acknowledged** | It pins the column structurally; the empirical proof is the gate case, which drives the eviction and the resume through the API and reads `app.db`. Both are named in the Resolution so a later reader knows which is which. |
| **SP5 · P3** — the page would treat `usage: {in: 0, out: 0}` as reported | **Fixed** | `transcript.ts`'s `usage()` answers `null` for a zero sum — the producer's rule, so a *durable* row written before it cannot hide the notice either — with a web test for both halves (`{in:0,out:0}` → dropped, `{in:512,out:0}` → a real cost) and the notice's sentence. |

### What the review's "could not verify without running" list found

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — green (664 tests).
- `npm test` (38) and `tsc --noEmit` — green.
- `scripts/verify.sh` end to end — green: **53 cases, 490 assertions**, both musl targets. `usage_of`'s
  `mode=ro` read works against a live `app.db` (the reviewer flagged it as the gate's first read-only WAL
  reader); the eviction-then-resume sequence answers 202 → `stopped`/`evicted` → 202 → `idle` as designed.
- Mutations, in the reviewer's order: deleting `part("cacheWrite")` — **was green, now red** (SP1's fix);
  `input == 0 && output == 0` → `input == 0` — red on the all-cache arm; `usage()` returning `Some` for a zero
  sum — red on `a_usage_of_nothing_is_no_usage_at_all`; `>=` → `==` — still green, which is why SP3 was only P3.
