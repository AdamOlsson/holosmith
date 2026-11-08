## Review

**Method.** A fresh reviewer read the brief `/tmp/holosmith/reviews/PER-96-review-brief.md` and the full artifact
`/tmp/holosmith/reviews/PER-96-diff.txt` (2080 lines, `8568f34` with every new file staged), then the touched
files in the tree, `AGENTS.md`, `docs/build.md`, `docs/notes.md` (cross-cutting sections and the new
`### Landmines: stopped-session expiry`), ADRs 0016/0019/0023/0025/0032/0038/0052/0053/0056/0065/0068/0069/0070,
`GLOSSARY.md`, `HolosmithSpec.md` item 15 and D3, and the HS-064 slice of
`docs/stories/phase-2-deployable-mvp.md`. Read-only: no edits, no builds, no gate run. The gate was green before
it started (87 cases, 1498 assertions, no `SKIP`) and the tree was committed (`98485a4`) so it could not move
under the review.

---

## Standards

Verified against the diff at `8568f34` plus the repo files named below. Tooling-enforced items (fmt/clippy-broken *code*, tsc) skipped; broken **doc links** are not tooling-enforced here (`cargo doc` is not in the gate).

**Correct**

- **The policy runs the delete flow, not a copy of it.** `expiry.rs:194-197` calls `crate::http::complete_deletion`; that name is re-exported once at `http/mod.rs:34`; `deletion` now ends at `sessions.rs:1037` with the same call. There is no second snapshot, no second `retained_for`, no second removal, no second event write — the only quantity that differs between the two callers is the `Trigger`, and `apply_trigger`'s fifth argument (`sessions.rs:928-957`) is used only as the operation name in a 500's log line, so `"delete"` → `trigger.as_str()` changes nothing observable for `DELETE`.
- **The `DELETE` request half is behaviourally untouched**: row read, `deleting` short-circuit with re-run and no second snapshot (`sessions.rs:1024-1035`), turn end then re-read (`1041-1053`). ADR-0023's order survives the split.
- **The refusal is real and atomic.** `Transitioner::apply` reads the row, looks the row up in the table and writes inside one mutex scope (`state.rs:441-470`), so `Trigger::Expire`'s `stopped`-only row (`state.rs:282-286`) cannot be raced by a resume; the `deleting`-sink and no-double-admission invariants were extended correctly (`state.rs:673-680`, `692-706`).
- **"An `error` row is never auto-deleted" is honestly documented as under-tested in-process.** `connection` is private (`db.rs:50-54`) and `set_status` writes `stopped_at` only for `stopped` (`repository.rs:315`), so `tests/expiry.rs:244-271` cannot isolate the predicate — and removing `status = ?1` makes the statement mis-bind, the sweep log-and-return, and the row survive, i.e. the test passes either way. The change says so in three places (`repository.rs:224-232`, `tests/expiry.rs:245-247`, ADR-0070 D4) and puts the isolation in the gate's `error` row with a stale clock (`scripts/verify.sh`, the `UPDATE … status = 'error' … stopped_at = ?` block). That claim is accurate, not self-serving.
- **Every removal assertion waits** (`wait_gone`, `expiry.rs:77-89`, used at 118 and 441-442); the negative tests sit on arms that return before `Deleter::spawn`.
- **Overflow is "keep for ever", never "delete now"** (`clock.rs:44-46`, `expiry.rs:66-68`, pinned by `expiry.rs:252-262`); `0` is unreachable (`config/mod.rs:597-605`).
- **The gate case owns its state and its `SKIP` cannot fire on the feature** (three `SKIP`s: daemon, image, `python3`).

**Findings**

- **should-fix** — `GLOSSARY.md:184` still enumerates **sixteen** triggers and omits `expire`, while its own sentence is "one per row of the transition table" and the table now has seventeen rows (`state.rs:282-286`, `Trigger::ALL.len() == 17`). `docs/notes.md` (`### Config and docs`) makes `GLOSSARY.md` "the vocabulary authority" and forbids a second spelling; HS-063 added `disk_quota` to this very list. Smallest fix: add `` `expire` ``.
- **should-fix** — `tests/expiry.rs:363-406`: `a_row_that_left_stopped_is_not_expired_even_after_the_pass_read_it` resumes the row at `:378-390` **before** the sweep at `:392`, so the row is never "a candidate when the pass reads it" as its comment claims; neither `expire`'s status re-read (`expiry.rs:184-193`) nor `Trigger::Expire`'s refusal is reachable, and the test stays green if you delete either guard or widen the row to `FromStates::Any`. Against `docs/notes.md` `### Tests`: "prove a new guard fires by removing the behaviour and watching the test go red" and "A test's claim can be half-strength and still green." Smallest fix: the interleaving is only buildable inside the crate — add a `#[cfg(test)]` test in `src/expiry.rs` that reads the row while it is stopped, resumes it through `Transitioner`, then calls `Expiry::expire(stale_row)` — and reword the comment to what the integration test actually proves.
- **nit** — `docs/notes.md:2273-2277`: "change `FromStates::OneOf(&[Status::Stopped])` to `Any` and **only that test** goes red" is false; `state.rs:535` `the_table_is_the_specs_table_row_by_row` and `state.rs:673-680` (the `lookup(Status::Deleting, trigger)` spot check) also fail on that mutation. The conclusion that `tests/expiry.rs` stays green is right; the word "only" is not.
- **nit** — `src/expiry.rs:249`: "Nothing is past a zero window" contradicts `cutoff(now, 0) == now` on the next line and ADR-0070 D1 ("a `0` here would mean 'remove every stopped session the moment it stops'").
- **nit** — `src/expiry.rs:205-216`: the `UNSAVED_WORK` refusal and every other error — including `apply_trigger`'s 500-class `internal` — both log at `info`, so a daily job that can never succeed is invisible at `warn`. The sibling policy logs the equivalent "cannot measure" at `warn` (`quota.rs:338`, `:349`, `:373`).
- **nit** — `web/src/main.ts:177-178`: the comment describing `RETAINED` is left orphaned after the constant was deleted, while ADR-0070 D8 claims "The HS-064 placeholder title (`RETAINED`, …) is deleted with the story it named."
- **nit** — `sessions/repository.rs:231` links ``[`expired_before_filter`]``, which does not exist (the constant is `EXPIRED_BEFORE_FILTER`, `repository.rs:117`); a broken intra-doc link nothing in the gate checks.
- **nit** — `GLOSSARY.md:295-296`: "It has two **halves and / two callers**:" breaks mid-phrase at ~75 columns inside a paragraph that otherwise runs ~110 — the scripted-edit artifact `docs/notes.md` (`### Config and docs`) warns about ("must be reflowed … not just appended to").
- **nit** — `web/src/main.ts:369-371`: `expiryText(now, stoppedTtlDays(...), now)` passes the same value as parameter 1 (`stoppedAt`) and parameter 3 (`now`); two same-typed `number` parameters make that call site unreadable (Primitive Obsession; judgement call).
- **nit** — `tests/expiry.rs:322-326` asserts `row.stopped_at == Some(now)` against a clock read once at `:281` while `Transitioner::apply` writes its own `clock::unix_now()` (`state.rs:470`): a run that crosses a second boundary flakes. The repo's precedents avoid it (`state_machine.rs:194` uses `is_some()`, `session_repository.rs:572` passes an explicit `now`).
- **nit** — `scripts/verify.sh`: "The container was gone already — a stopped session's `--rm` run removed it" is not what happened in that case — `idle_stop_min = 0`, and the row was rewritten from `idle` by SQL, so the container is removed by the delete flow's own `backend.delete` — which is a *stronger* claim the case could assert (`docker inspect s_$id`) and does not. No case asserts the job's `info` line either, whereas `case_disk_quota` asserts its `holosmith::quota` line.

Axis line: **Standards — 10 findings (2 should-fix, 8 nit); worst: `GLOSSARY.md:184` still lists sixteen triggers, omitting `expire`.**

## Spec

Source: `docs/stories/phase-2-deployable-mvp.md` HS-064 (five acceptance bullets), `HolosmithSpec.md:260` (item 15 / D3) and item 9's delete flow.

No issues found.

What I checked, and why it holds:

- Bullet 1 (`stopped_at` set/cleared) is pre-existing and witnessed by `tests/expiry.rs:273` (six reasons into `stopped`) plus `state_machine.rs:220`.
- Bullet 2 (daily job, `stopped_ttl_days`, HS-043 delete flow): `TICK` is one day (`expiry.rs:55`, pinned by a unit test), the candidates are one repository statement (`repository.rs:117`), and "using the delete flow" is literal — `complete_deletion` is the same function the operator's `DELETE` reaches, so the snapshot order, the keep-rules, the one-transaction row-plus-log removal and the stream close are not re-decided. `case_stopped_expiry` measures the kept branch and WIP ref.
- Bullet 3 (snapshot first; failure skips and retries): `force` is a literal `false` (`expiry.rs:194-197`), `UNSAVED_WORK` returns before any row write, and the test asserts the row's `status`/`stopped_at` are untouched *and* still in `expired_before`'s answer (`tests/expiry.rs:227-241`) — the retry is the candidate set, as the spec's "retried the next day" requires.
- Bullet 4 (`error` never auto-deleted): the predicate plus the `NULL` clock plus the re-read, with the gate isolating the predicate.
- Bullet 5 (days remaining on each stopped session): `stopped_at` is on the wire (`sessions/mod.rs:120`, `:228`) and `stopped_ttl_days` is on the list (`sessions.rs:621`), neither contradicting the API table (`HolosmithSpec.md:297-305` does not pin the list body, and ADR-0017's `network_default` is the precedent); the sentence is drawn by one pure function both render paths call, never says "deleted", and a non-default-config test (`session_list.rs:155-176`) rules out a hard-coded `7`.
- No scope creep: the only surface added beyond the bullets is the list key the last bullet needs, documented in ADR-0070 D8. No migration, correctly (`stopped_at` is column 13 since HS-007).

Axis line: **Spec — 0 findings; nothing to fix.**

**Merge verdict: OK with notes** (the two should-fix items are a stale glossary enumeration and one test whose name/comment overclaim — neither blocks the destructive path, which I traced end to end).

---

## Resolution

| # | Finding | Resolution |
| --- | --- | --- |
| S1 | `GLOSSARY.md`'s **Trigger** entry enumerates sixteen triggers and omits the new `expire`, while its own sentence is "one per row of the transition table" (now seventeen) | **Fixed.** `expire` added to that list — the same edit HS-063 made for `disk_quota`, in the vocabulary the codebase reads as authority. Mutation-checked in the sense that matters: the sentence and `Trigger::ALL` are now equal, and `state.rs`'s `every_trigger_has_a_word_and_every_word_is_the_tables` is what keeps them so. |
| S2 | `tests/expiry.rs`'s `a_row_that_left_stopped_is_not_expired_even_after_the_pass_read_it` resumes the row **before** the sweep, so it never reaches the pass's status re-read nor `Trigger::Expire` — it stays green if either is deleted | **Fixed, and split in two.** The integration test is renamed `…is_not_a_candidate_at_all` and now claims only what it proves (the statement's `status = 'stopped'` predicate answers before any policy code). The interleaving the name promised is a `#[tokio::test]` in `src/expiry.rs` — it reads a stale candidate, resumes it through `Transitioner`, hands the **stale row** to `Expiry::expire`, and asserts the stub recorded no `Snapshot` **and** no `Delete`. That is in-crate because `expire` is private and a whole `sweep` cannot be interleaved. Mutation-checked: deleting the status re-read makes it red (the stub then snapshots a session that is being re-provisioned), and the test's comment says plainly that the table row's own witness is `tests/state_machine.rs`, not this. |
| S3 | `docs/notes.md` claims that widening `Trigger::Expire` to `FromStates::Any` turns "only that test" red | **Fixed.** The sentence now names the three witnesses (the new row-by-row test, the table's own row-by-row test, and its `lookup(Deleting, …)` spot check) and says that is the point rather than a defect. |
| S4 | `src/expiry.rs`'s cutoff test says "nothing is past a zero window" beside an assertion that `cutoff(now, 0) == now`, contradicting ADR-0070 D1 | **Fixed.** The comment now states the arithmetic: a zero window puts the cutoff at "now", the comparison is strict so a row stopped exactly now is not past it, and the config refuses `0` — so this is totality, not a mode. |
| S5 | The `UNSAVED_WORK` skip and every other refusal both log at `info`, so a daily job that can never succeed is invisible at `warn` | **Fixed.** The snapshot skip stays `info` (it is a decision, and it says "a candidate again tomorrow"), and every other ending — a state the table refused in the window, a repository or teardown failure — is now `warn` with the code, the status and the sentence, matching `quota.rs`'s "cannot measure" arms. |
| S6 | `web/src/main.ts` keeps the comment that described the deleted `RETAINED` placeholder | **Fixed.** The comment is deleted (ADR-0070 D8's claim is now true of the file). |
| S7 | `sessions/repository.rs` links `[expired_before_filter]`, which does not exist — the constant is `EXPIRED_BEFORE_FILTER` | **Fixed.** The intra-doc link names the constant. (Worth recording: `cargo doc` is not in the gate, so a broken doc link is only ever a review finding.) |
| S8 | `GLOSSARY.md`'s new **Delete flow** paragraph breaks mid-phrase ("two **halves and / two callers**") at ~75 columns inside a paragraph that runs ~110 | **Fixed.** The paragraph is reflowed at the file's own width. |
| S9 | `expiryText(now, stoppedTtlDays(...), now)` at the patch site passes the same value as parameters 1 and 3, which reads as a mistake | **Fixed, minimally.** Named apart at the call site (`stoppedAt` and `now`), with a comment saying why they are equal there: the transition just wrote the clock, so the stop instant is the frame's arrival. The function's signature stays — `stoppedAt` and `now` are genuinely different numbers on the row path, and one function for both paths is the rule that matters more. |
| S10 | `tests/expiry.rs` asserts `stopped_at == Some(now)` against a clock read before `Transitioner::apply` reads its own, so a second boundary flakes it | **Fixed.** The claim is now a window (`now ≤ stopped_at ≤ clock::unix_now()`), which is "the transition recorded when it happened" without the race — the same shape `docs/notes.md`'s `last_active_at` lesson prescribes. |
| S11 | `case_stopped_expiry`'s comment asserted a mechanism (`--rm`) that did not happen, and neither the container nor the job's own log line was asserted | **Fixed.** The comment now says the case asserts an outcome ("nothing left behind") and names the one resource only `backend.delete` removes, and the case asserts both the container's absence (`docker inspect s_<id>`) and the job's `holosmith::expiry` line — `case_disk_quota`'s idiom for its policy's line. |

Both axes were re-read after the fixes. The Spec axis had **no findings** and none appeared. The two should-fix
items were a stale vocabulary list and a test whose name promised more than it proved; neither touched the
destructive path, which the reviewer traced end to end and confirmed (one code path for both callers, the
snapshot before any row write, `force` a literal `false`, and the `stopped`-only table row atomic with the write).

**Mutation checks run in this session, and reverted before the gate run:** widening `Trigger::Expire` to
`FromStates::Any` (turns `the_expirys_row_admits_stopped_and_nothing_else` red), `Math.ceil` → `Math.floor` in
`expiryText` (turns the node test red), and deleting the pass's status re-read (turns the new in-crate
interleaving test red). Three green tests that were checked to be able to fail.
