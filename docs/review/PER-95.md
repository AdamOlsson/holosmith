## Review

**Method.** Read the brief, the skill, the full artifact `/tmp/holosmith/reviews/PER-95-diff.txt` (2393 lines, `c8eb08c...HEAD`), and the touched files in the tree: `crates/holosmith/src/{quota.rs,ladder.rs,stop.rs,container.rs,preflight.rs,events/mod.rs,http/app.rs,sessions/state.rs,commands/serve.rs,logging.rs}`, `tests/{quota.rs,disk.rs,container.rs}`, `web/src/{transcript.ts,main.ts,style.css}`, `web/test/transcript.test.ts`, `scripts/verify.sh` (`case_disk_quota` + its helpers), `docs/{build.md,notes.md}`, `docs/adr/{0018,0019,0069}`, `docs/stories/README.md`, `docs/stories/phase-2-deployable-mvp.md` §HS-063, `HolosmithSpec.md` item 14, `GLOSSARY.md`, and the prior review artifacts in `docs/review/` for convention. Read-only: no edits, no builds, no gate run (the 14-minute run in that tree is untouched). Findings below are P2 unless marked.

---

## Standards

**S1 · P2 — the new transcript block has no stylesheet rule at all.** `web/src/main.ts:607-615` creates `article.session-error`, `div.session-error-label`, `div.session-error-text`; `web/src/style.css` has no `.session-error*` rule, while every sibling block does — `.steer`/`.steer-label`/`.steer-text` (`style.css:287-302`) and the block this one copies, `.turn-error`/`-label`/`-text` (`style.css:306-318`). So the frame renders as three unstyled divs: no left marker, no quiet uppercase label, no `overflow-wrap`. This is the repository's own recorded finding class — `docs/review/PER-49.md:37-39` flagged exactly this for `.steer` and it was fixed (`PER-49.md:159`). *Smallest fix:* copy `.turn-error`'s three declarations under a `.session-error` heading, beside them.

**S2 · P2 — the new kind `error` is the DOM's own `EventSource` event name, and the page now routes it into the transcript.** `web/src/transcript.ts:132-136` adds `ERROR` to `TRANSCRIPT_EVENTS`; `web/src/main.ts:1028-1043` registers one listener per entry, so `source.addEventListener("error", …)` now receives both the new frame *and* the browser's connection-error event. It is benign today only because `parseFrame` wraps the parse (`transcript.ts:232-236`), so `JSON.parse(undefined)` throws into the `catch` and returns `null` — verified, not assumed. But the two meanings are now indistinguishable at the one place a "connection lost" notice would be written, and the wire word `error` also reads against `Frame.type: "error"`, which is `turn.error` (`transcript.ts:59`), and against the *state* word `error`. *Smallest fix:* guard the shared handler (`if (!(event instanceof MessageEvent)) return;`), or name the kind something not reserved — spec item 14 fixes the **code** `disk_quota`, not the SSE event name.

**S3 · P2 — the reason-word enumeration is now short by one.** `docs/notes.md:34-36`: *"The six reason words for a climbed stop are `forced_abort`, `deleted`, `evicted`, `idle_stop`, `turn_timeout` and `budget_exceeded`; a seventh, `pi_exit`, is not a climb at all…"*. `disk_quota` is a seventh climbed-stop reason (`quota.rs:383`, through `Ladder::end_turn`), so both the count and the list are wrong. *Fix:* add `disk_quota` and renumber `pi_exit`.

**S4 · P2 — the sentence the change invalidated is the one in the notes.** `docs/notes.md:625-629`: *"`stoppedNotice` is now **two words wide** … Adding a third word means a third sentence and a test that it is not another's."* The diff makes it three (`transcript.ts:552-554`) and adds precisely that test (`transcript.test.ts:409-424`); the rule was followed, the count was not updated. *Fix:* "three words wide" plus the `disk_quota` pair.

**S5 · P2 — the same off-by-one inside the diff.** `web/test/transcript.test.ts:441-442`: the comment over the loop reads *"The four reason words the machine writes on its own, and the two a client request does"*, and the loop it now heads has seven entries (the diff added `disk_quota` to it). Five + two. *Fix:* "five".

**S6 · P2 — the sixth event kind is missing from both documents that enumerate kinds.** `docs/notes.md:422-436` is the transcript's contract table (`message.delta` … `turn.error`, `session.status`) and has no `error` row; `docs/adr/0018-the-transcript-contract-and-one-block-per-message.md:22-25` and `docs/adr/0019-the-turn-runner-and-the-backends-events.md:80-83` each carry an "**Amended by ADR-0043:** there is a fifth kind" line and neither is amended for the sixth. ADR-0069 D5 cites ADR-0018's additive rule but does not extend ADR-0018's own list. This is the repository's recorded convention, flagged twice and fixed twice: `docs/review/PER-67.md:93` (S1) and `docs/review/PER-49.md:157` (S1). *Fix:* one table row in `notes.md` and one "Amended by ADR-0069" line in each ADR.

---

## Spec

**SP1 · P2 — acceptance bullet 1's interval has no witness that can fail.** `quota.rs:81` (`TICK = 5 * 60`) is asserted nowhere, and `Quota::run` (`quota.rs:229-236`) is never driven: every crate test calls `sweep()` directly and `case_disk_quota` relies on the first pass. Mutating `TICK` to `5` seconds or `5 * 3600` leaves the whole suite and the gate green, so "every 5 minutes" is implemented but unmeasured. (The sweeper's `TICK` shares the gap — ADR-0025's precedent — so this is a witness gap, not a behaviour bug.) *Smallest fix:* one line beside `the_comparison_is_strict_and_counts_gibibytes`: `assert_eq!(TICK, Duration::from_secs(300));`.

**SP2 · P2 — `build.md` claims two assertions the gate case does not make.** `docs/build.md:426-428`: *"that `GET …/events/history` carries an `error` frame with `"code":"disk_quota"` and the two numbers"*. `scripts/verify.sh:7347-7355` greps the page for `"type":"error"` only; the code and both numbers are asserted on the **stream** (`verify.sh:7335-7339`). The page's payload is an escaped JSON string, so that sentence is not even greppable as written — and `verify.sh:7218-7222`'s own comment says the page keeps only the `type` assertion. *Fix:* move "with the two numbers" to the stream clause.

**SP3 · P2 — the `--one-file-system` landmine names a witness that does not exist.** `docs/notes.md:2253-2257` says the flag would *"leave a nested mount out of a worktree's size"* and that *"the one test that uses the machine's own `du` builds a tree with both directories in it and asserts the sum"*. `tests/quota.rs:570-589` builds `target/`, `node_modules/` and `src/` on **one** filesystem and asserts `size >= 128` KiB of 192 KiB — which `du --one-file-system` answers identically. The test does catch per-directory exclusion; it cannot catch that flag. *Fix:* either drop the flag sentence, or add a shim that records its argv and assert `-sk --` with no `--one-file-system`.

**SP4 · P2 — the `Ending::Asked` arm can log a false sentence.** `quota.rs:398-404`: after rung 1 succeeds, the second re-read treats any status other than `Idle` as *"the session left the row before the cap could stop it"*. A `prompt` accepted in that window moves `idle → running`, and the row has **not** left — it is live with a turn nobody aborted (the next tick's, five minutes later). The `_` arm immediately below (`quota.rs:409-414`) already names the state. *Fix:* carry `state = idle.status.as_str()` into this arm's line too. (Delete/resume races *are* handled correctly: `apply` refuses, the container is not stopped, nothing is reported.)

**SP5 · P2 — one gate assertion cannot fail, and could false-fail.** `scripts/verify.sh:7356-7360` fails the case if the history page contains `"status":"error"`. That page carries events, whose payload keys are `from`/`to`/`step`/`reason` — `"status"` never appears there, so the check cannot fire for the reason it states; and a `tool.end` payload *does* carry `status` (`docs/notes.md:432`), so it could fail on a tool call. The row's `stopped` is already asserted at `verify.sh:7292-7299`. *Fix:* delete it, or assert the row's `"status":"stopped"` on `GET /api/sessions/{id}`.

**SP6 · P2 — D2's test is narrower than the claim it carries, in two ways.** `crates/holosmith/src/container.rs:806-816` filters mounts to `!mount.read_only`, so a *shared read-only* cache directory would keep the test green, while bullet 5 says "no cache directory is shared between sessions" and the test's own doc (`:769-770`) says "no writable host path". Second, the test reads `mounts.all()`, not `run_args` — so `docs/adr/0069…:100-101` ("a unit test over `SessionContainer::run_args`") and `docs/stories/README.md:189` ("a test over two sessions' `docker run` vectors") overstate it: a `-v` added outside `Mounts` would be invisible to it. *Fix:* assert the shared set over **all** mounts (the satisfiable exact answer is `{git_dir, git_dir/hooks, git_dir/config}`, the last two read-only) and say "over `Mounts`" in the ADR.

---

## What is witnessed, and what is not

| Claim | Witness | Verdict |
|---|---|---|
| **1.** watchdog runs `du` on each live session's worktree, every 5 minutes | `tests/quota.rs:305-311` + `verify.sh:7301-7311` (the stand-in's logged `-sk -- <that session's worktree>`, and *not* the data root); the spawn is witnessed indirectly by the gate's restart-then-stop. The **interval** is witnessed by nothing (SP1) | **partly** |
| **2.** over the cap → `error` event with code `disk_quota`, aborts the turn, stops the session | `an_idle_session_over_the_cap_is_stopped_and_reported`, `a_running_turn_is_forced_stopped_after_the_grace` (rung 1 asked *and* rung 2 forced), `a_turn_that_aborts_politely_is_stopped_as_an_idle_session`, `the_event_is_at_the_wire`; gate asserts the stream frame's `event: error` + `code` + both numbers + the log line | **yes** |
| **3.** the stop is `stopped`, not `error` | row + `status_reason` in the crate tests, `verify.sh:7292-7299`, the resume's `202` in both (`a_quota_stop_is_resumable`, `verify.sh:7362-7374`) | **yes** |
| **4.** the cap includes `target/` and `node_modules`; docs say to raise it | the docs half is prose (`GLOSSARY.md:229-243`, `config/mod.rs:231-235`, `build.md:418-430`) and can only be read; the inclusion half is `tests/quota.rs:570-589` — real, but it cannot catch `--one-file-system` (SP3) | **partly** |
| **5.** no cache directory shared between sessions, asserted by a test | `container.rs:782-846` — real for writable host paths; a shared read-only directory would not fail it (SP6) | **partly** |
| **`case_*` rule** (`docs/build.md:523-526`) | `case_disk_quota` exists, is in `serve_cases` (`verify.sh:436`), measures real things, needs no earlier case, restores the shared boot (`verify.sh:7375-7384`), and both `SKIP` conditions are daemon/image facts that do not hide the bug the case exists to catch | **yes**, with SP2/SP5 as the caveats |

The other repository rules hold: ADR-0069 exists with a line at `docs/stories/README.md:189` and the count moved 68→69; `docs/stories/**` is otherwise untouched; the client-visible *sentence* for the row's reason is a pure `transcript.ts` function with a node test (the event's own `message` is server-composed, which ADR-0069 D5 decides and ADR-0068's `Cow` message set the precedent for).

**Merge verdict: OK with notes.** No P0/P1. The behaviour matches item 14 and D2, the ladder/table/event wiring is right, the two failure arms of `du` and `session_disk_soft_gb = 0` are handled and tested, and the gate case is a real witness. The items above are one stylesheet omission (S1), one overloaded kind name (S2), five prose/count drifts of the kind this repository's last reviews kept finding (S3-S6, SP2, SP3), and two assertions worth tightening (SP5, SP6).
---

## Resolution

All twelve findings were fixed in the same session. Nothing was dismissed.

| # | Finding | Resolution |
| --- | --- | --- |
| S1 | `article.session-error` and its two children had no stylesheet rule | **Fixed.** `web/src/style.css` gains `.session-error`, `-label` and `-text` beside `.turn-error`, with the same marker/quiet-label/`overflow-wrap` (the class PER-49's review established). |
| S2 | The kind `error` is also the platform's `EventSource` connection-event name, so one listener sees both | **Fixed, on the client.** `main.ts`'s frame listener now returns unless `event instanceof MessageEvent`, so a connection failure is never parsed as a frame; a server-chosen `error` frame always arrives as a `MessageEvent`, which is what makes the guard exact. The wire word stays `error`, because item 14 names it and only the *code* (`disk_quota`) is the API the spec fixes. `docs/notes.md`'s web section records the collision. |
| S3 | `docs/notes.md` counted six climbed-stop reason words; `disk_quota` makes seven | **Fixed.** Seven named, `pi_exit` renumbered to the eighth (it is not a climb). |
| S4 | `docs/notes.md` said `stoppedNotice` is "two words wide" | **Fixed.** Three words, `disk_quota`'s sentence named, and the `MessageEvent` guard recorded in the same bullet. |
| S5 | The test's comment said "the four reason words … and the two a client request does" over a seven-entry loop | **Fixed.** Five. |
| S6 | The transcript's contract table and ADR-0018/ADR-0019 have no line for the sixth kind | **Fixed.** One row in `docs/notes.md`'s table, and an "Amended by ADR-0069" paragraph in each ADR, in the shape ADR-0043's amendment already had. |
| SP1 | `TICK`'s five minutes had no witness that could fail | **Fixed.** `the_tick_is_the_specs_five_minutes` asserts `TICK == 300 s` and that `SETTLE < TICK`; mutating `TICK` to five seconds now goes red. |
| SP2 | `docs/build.md` credited the history page with two assertions the case makes on the stream | **Fixed.** The two numbers are the stream's clause (the page's payload is an escaped string); the page is asserted by kind. |
| SP3 | The `--one-file-system` landmine named a witness that cannot see the flag | **Fixed.** A recording shim in `tests/quota.rs` pins the argument vector (`-sk -- <path>`) and the landmine says which test sees which half. Mutating `measure` to add the flag makes that test red — checked, not assumed. |
| SP4 | The `Ending::Asked` re-read logged "left the row" for a row that had merely been prompted | **Fixed.** The arm splits: a row that is not `idle` logs its actual `state`, and only a missing row says it left. |
| SP5 | `case_disk_quota`'s `"status":"error"` page check could not fire on the bug it named and could false-fail on a `tool.end` | **Fixed.** Removed; the row's `stopped` is asserted where it is read, and the comment says why a page cannot carry that needle. |
| SP6 | D2's test filtered to writable mounts, and the prose claimed `run_args` while the test read `Mounts` | **Fixed.** The test now parses the two `docker run` vectors' `-v` arguments and asserts the shared set is exactly `{.git, .git/hooks, .git/config}` — so a shared read-only path fails it too, and a `-v` added outside `Mounts` is visible to it, which makes the ADR's and the index's wording true. |

Two mutation checks were run to believe the two new non-doc claims: `over`'s `>` → `>=` turned
`the_comparison_is_strict_and_counts_gibibytes` red, and adding `--one-file-system` to `measure`'s arguments turned
`a_measurement_that_cannot_run_stops_nothing_and_asks_for_nothing_extra` red. Both mutations were reverted before
the gate run.

The rest of the review — both ladder rungs, the `du` failure arms, `session_disk_soft_gb = 0`, the settle wait, the
table row, the event's shape — was confirmed as witnessed, and its acceptance table (five bullets: three yes, two
partly) is why SP1, SP3 and SP6 existed. After these fixes, bullets 1, 4 and 5 have a witness that can fail.
