# Review — PER-51 (HS-019, the delete flow)

- **Fixed point:** `b49f2cd`
- **Reviewed change:** `3d5c415`, `7c4ea02` (`b49f2cd..HEAD` at the time of review)
- **Reviewer:** a fresh `reviewer` subagent running `/home/adamolsson/.agents/skills/code-review/SKILL.md`,
  read-only, no shell. Its claims were checked by the orchestrator; the Resolution table records what was
  fixed and what was dismissed.
- **Verdicts:** Spec — OK with notes (one unpinned consequence of a refusal on a running session).
  Standards — OK with notes (four documentation-vs-code contradictions and one duplicated
  `StateError` → `ApiError` map).

This review is saved verbatim below this header. The `## Resolution` section at the end was appended
afterwards.

---

## Spec

**Correct (verified by reading code and tests)**

- 202 + `deleting` + row removal + stream close: `http/sessions.rs:683-740` applies `Trigger::Delete`, answers `(ACCEPTED, Json(deleting))`, and hands the row to `Deleter::spawn`; `deleter.rs:75-121` revokes, calls `SessionBackend::delete`, then `SessionRepository::remove` (`sessions/repository.rs:217-231`, row + log in one `unchecked_transaction`), then `StreamRegistry::close` (`events/registry.rs:73`), which the SSE body honours. Proved by `tests/delete.rs:168` (202 body + row gone + one/list 404) and `tests/delete.rs:213` (the stream ends) — and that test is **not** vacuous: `Sse::drain` runs `next_block`, which *panics* on timeout (`tests/support/mod.rs:409-420`), so returning from `drain` really is the server ending the response. `scripts/verify.sh:1005` repeats the round trip against the shipped artifact with `wait_for_gone`.
- Abort first if running, snapshot before removal: `http/sessions.rs:698-727` calls `Ladder::end_turn` (no resume, `ladder.rs:151-170`) then `backend.snapshot`; `tests/delete.rs:353` pins the order `clear_queue → abort → stop → snapshot → delete`, `tests/delete.rs:208` pins `[Snapshot, Delete]`.
- 409 `unsaved_work` unless `?force=true`: `error.rs:253-261`, `http/sessions.rs:712-727`; `?force=true` forgives but still attempts (`tests/delete.rs:282-296` asserts `[Snapshot, Delete]`); `force_requested` (`http/sessions.rs:747-753`) matches only the literal, and `tests/delete.rs:1030` enumerates the near-misses. Spec item 9 and the API table are honoured.
- Idempotency: absent → 404, `deleting` → 202 and the task re-runs with no second snapshot (`http/sessions.rs:691-698`, `tests/delete.rs:312-328`), after removal → 404 (`tests/delete.rs:336-350`). Never a partial state.
- Dummy-key hook: `deleter.rs:147-154` exists and is called first (`deleter.rs:76`), which is what the acceptance asks for ("the hook exists here").

**Finding: P2 — the refusal is only "reversible" for a session that is not running, and no test covers the path where it is not.**

- `crates/holosmith/src/http/sessions.rs:698-713` with `crates/holosmith/src/ladder.rs:224-271`.
- Story acceptance is "**As** the operator **I want** delete to be safe and reversible until it is not **so that** a mis-tap cannot destroy uncommitted work", and the new glossary entry asserts the refusal is harmless. For a `running` row the handler first spends `abort_grace`, and rung 2 writes `running → stopped` (reason `deleted`) and calls `backend.stop` **before** the snapshot runs. If the snapshot then fails, the answer is 409 — but the row is now `stopped` with `status_reason = "deleted"` and the container is dead. A mis-tap has therefore destroyed the running turn's unsaved work and left a session that says "deleted" but is not deleted. ADR-0023 §2 admits the loss ("the work could not be saved"), so the loss is owned; what is **not** owned is the row-state change, and nothing proves it: both `failing_snapshot()` tests start from `Idle` (`tests/delete.rs:250` and `:288`), so `a_failing_snapshot_refuses_the_delete_and_leaves_the_row_alone` (`tests/delete.rs:245`) proves its name only for the state it happens to use, and `the_ladder_ends_a_turn_without_bringing_the_session_back` (`tests/delete.rs:384`) uses a *succeeding* snapshot.
- Smallest fix: one test — `failing_snapshot()` + a `running` row — asserting the 409 **and** `row.status == Stopped` / `status_reason == Some(DELETED)`, so the behaviour is pinned rather than assumed; plus the glossary wording below.
- Why not P1: spec item 9 mandates `abort if running; snapshot` in that order, so the code is spec-faithful; the gap is an unpinned, undocumented consequence.

**Scope:** no creep found. The touched extras (`ApiError::unknown_session`, the provisioner's stop check, the writer's drop rule, `DELETED`) are each forced by an acceptance bullet or by the row's removal, and ADR-0023 owns them.

## Standards

**Correct:** `docs/build.md:143-150` and `scripts/verify.sh:290-293` add the three required `case_*` functions and array lines; `docs/stories/README.md:114-141` updates the ADR count and index to twenty-three; ADR-0023 exists and is linked; `docs/adr/0008` (one envelope, new sentence on the same code), `0016` (`Trigger::Delete` already in the table, `state.rs:229-233`), `0019`/`0020` (backend emits, ladder owns the row order) are respected; `0015`'s single `delete` trait call is used rather than five host-side steps. `cargo fmt`/`clippy` claims not re-checked (see Verification).

**Finding: P2 — the glossary (normative) was not updated for the ladder's new shape, so it now describes behaviour the code no longer has.**
- `GLOSSARY.md:116-118` ("Abort ladder"): "rung 2 … kills the container, records the reason, **and resumes the session at once**. … the reason rung 2 records is the caller's — `forced_abort` for an abort, and the turn timeout's and the token budget's own words."
- The diff splits the ladder precisely so that rung 2 does **not** resume: `ladder.rs:151-170` (`end_turn`, `Ending::{Asked,Forced}`), `ladder.rs:136-142` (`climb` = `end_turn` + resume), and the delete passes a third reason word, `deleter::DELETED` = `"deleted"` (`deleter.rs:39`, used at `http/sessions.rs:699`). `GLOSSARY.md:144-146` ("Stopped": "reached … from a killed `running` turn by the abort ladder; never a dead end") is silent on the delete-killed turn that *is* a dead end until it is forced past.
- Smallest fix: one sentence each — "`Ladder::end_turn` is the rungs without the way back; the delete climbs it with the reason `deleted` and does not resume" — and add `deleted` to the reason list.

**Finding: P2 — `GLOSSARY.md:173` ("the row is left exactly as it was") is false, and it is the sentence ADR-0023 just corrected.**
- `GLOSSARY.md:168-176`, sentence "A snapshot that fails answers 409 **unsaved work** unless the request carried `?force=true`, and the row is left exactly as it was."
- ADR-0023:12-15 in this same change says the opposite ("a refusal leaves the session where the operator found it — a running turn may have been ended by step 2"), and `http/sessions.rs:698-713` does exactly that. The two documents in one diff disagree; the normative one is the wrong one.
- Smallest fix: reuse ADR-0023's wording ("…leaves the session where the operator found it; a running turn may already have been ended, which the delete's abort step did on purpose"). While there: `_Avoid_: Removal, …` (line 176) is contradicted by the machine names the entry itself introduces (`Deleter::remove`, `remove_row`, `SessionRepository::remove`) — either list `Deleter::remove` as the machine name of the task half or drop "Removal" from the avoid list.

**Finding: P2 — ADR-0013 still documents the invariant this diff deleted.**
- `docs/adr/0013-event-log-single-writer-and-seq-counter.md:108-111`: "That includes an event naming a session that does not exist — there is no counter to bump, so the batch is refused with `RepoError::UnknownSession` rather than inventing a seq."
- The variant is gone (`events/mod.rs`, removed hunk) and the behaviour is inverted: `events/repository.rs:70-84` now drops the event and commits the rest. ADR-0013 is the writer's canonical document and ADRs are precedent (`PROMPT.md:10`), so a reader — or HS-022/HS-023 — reads a rule the code contradicts.
- Smallest fix: replace those two sentences with one pointing at ADR-0023's drop rule (the batch commits, the gone session's event is dropped, `debug`-logged).

**Finding: P2 — ADR-0023's closing consequence is false and contradicts its own §2.**
- `docs/adr/0023-the-delete-flow.md:146-147`: "The delete produces no `stopped` session, so HS-020's eviction is still the first producer of that state."
- A delete of a running session produces one, via rung 2 with reason `deleted` (ADR-0023:33-38 of the same file; `tests/delete.rs:402-406` asserts exactly `Status::Stopped` + `Some("deleted")`). HS-020 and HS-022 will read that bullet as a constraint that is not true.
- Smallest fix: "The delete can leave a `stopped` row (a forced rung 2 with reason `deleted`), but no successful delete leaves one; HS-020's eviction is still the first *producer of a stop the operator asked for*."

**Finding: P3 — the deleted-streams rationale states a benefit the same change makes unreachable.**
- `crates/holosmith/src/deleter.rs:19-21`: "4. **Close the session's streams.** Last, so a client already watching has had its chance at the `deleting` frame."
- In both configurations this build ships, the `deleting` event never reaches a subscriber: the stub's `delete` returns immediately and the Docker backend's `Unsupported` falls through to `remove_row` (`deleter.rs:76-100`), so the row and log are gone well inside the writer's 25 ms window — and `insert_batch` then drops the event (`events/repository.rs:70-84`). `tests/delete.rs:228-236` concedes it: "Which frames arrive first is not: the `deleting` event is committed by the writer on its own tick". (The ordering does matter for a backend slower than one tick, which is worth saying instead.)
- Smallest fix: reword to "last, so that a backend whose teardown outlives the writer's 25 ms tick still lets a watching client see `deleting` before the stream ends".

**Finding: P3 (smell: Duplicated Code / Repeated Switches) — the third copy of the `StateError` → `ApiError` map.**
- `crates/holosmith/src/http/sessions.rs:756-769` (`apply_delete`) is `apply_resume` (`:628-641`) with two words changed: the same `Unreported → Ok`, `Illegal → re-read the row and refuse with its state`, `UnknownSession → unknown_session`, `other → internal` arms. `apply_prompt` (`:779-800`) is a fourth variant. Every new trigger adds a fifth copy.
- Smallest fix: one helper `async fn apply_trigger(state, id, trigger, operation: &'static str, refusal: &'static str) -> Result<(), ApiError>` that `apply_resume`/`apply_delete` call; leave `apply_prompt` alone (its retry-once loop is genuinely different).

## Verification

**Verifiable by reading (code + tests above):** acceptance bullets 1, 2 (abort-first and snapshot-before-removal halves), 3, 4, and the existence of the key hook (bullet 5). Bullet 2's "remove the session directory and network" is **not** verifiable: it is delegated to `SessionBackend::delete`, which the stub only records (`backend/stub.rs`, `Call::Delete`) and the Docker backend refuses outright (`backend/docker.rs:75-81`); the actual teardown is HS-024's, as ADR-0023 §3 states.

**Not verifiable without running:**
- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test` — must be run by the supervisor (`scripts/verify.sh` runs all three, plus the artifact build).
- `scripts/verify.sh`'s serve phase, i.e. whether `case_delete_session`'s strict `"sessions":[]` assertion holds at that point in the run. I read the array order (`verify.sh:262-295`), the single create (`:465-495`) and the `max_live_sessions = 1` config (`:1181`) and it *should* hold — only a run proves it.
- All timing-dependent claims: whether a live stream ever receives the `deleting` frame (no test asserts it; I argue it usually cannot), and `a_delete_closes_the_sessions_stream`'s `drain(PATIENCE)` fitting inside the bound.
- The plan's demo step 2 (Docker backend: 409 `unsaved_work` → `?force=true` → 202 → gone), which no test covers because no test uses `Backends::Docker` on this route.

**Artifact limitation:** the saved diff at `/tmp/holosmith/reviews/PER-51-diff.txt` is **not** identical to `b49f2cd..HEAD`. Its `docs/adr/0023-the-delete-flow.md` hunk is `+1,145`, while the file at HEAD is 148 lines: the paragraph at `docs/adr/0023-the-delete-flow.md:12-15` was rewritten after the diff was captured (it now owns "a running turn may have been ended by step 2", where the diff still read "a refusal leaves the session exactly where it was"). Everything else I spot-checked (`deleter.rs`, `http/sessions.rs`, `events/repository.rs`, `sessions/repository.rs`, `ladder.rs`, `tests/delete.rs`, `verify.sh`) matches HEAD. All findings above are against the tree at `7c4ea02`, which `watchdog_diff` confirms is clean and equal to the checkout.

**Merge verdict: OK with notes.** No P0/P1: the flow is spec-faithful, the tests are real (and the stream test is not vacuous in the way it looks), and the idempotency rule is honoured by row state. The four P2s are documentation-vs-code contradictions inside this change's own new documents (glossary twice, ADR-0013, ADR-0023's closing bullet) plus one unpinned acceptance path; each is a one- or two-line edit, and I would take them before release because ADRs and the glossary are the repository's precedent (`PROMPT.md:10`).

No files were edited.
## Resolution

No blocker. One spec finding and six standards findings; six fixed, one already fixed when the review
landed. The reviewer's own note that its artifact was stale is right: two documentation edits (ADR-0023's
"before it" paragraph and ADR-0013's supersession note) were made while it was reading, so its finding
about ADR-0013 describes a state the tree had already left. Everything was re-checked against `cd85572`
afterwards, and the diff artifact was regenerated for the record.

| Finding | Status | How / why |
| --- | --- | --- |
| **Spec P2** a failed snapshot on a `running` session has already ended the turn and (with rung 2) written `running → stopped` with reason `deleted`, so the 409 is not the harmless answer the glossary claimed — and no test covered it | **fixed** | `a_failing_snapshot_on_a_running_session_stops_the_turn_and_still_refuses` drives a running row, deletes, and asserts the 409 envelope **and** `status == stopped`, `status_reason == Some("deleted")`, the session still present, and the recorded order `clear_queue → abort → stop → snapshot` with no `delete`. The behaviour is spec item 9's order ("abort if running; snapshot"), so it is pinned rather than changed — and the glossary now says so. |
| **Standards P2** the glossary's abort-ladder entry said rung 2 "resumes the session at once", which `end_turn` exists to avoid | **fixed** | The entry names both entry points (`Ladder::climb` resumes, `Ladder::end_turn` does not) and lists all three reasons, `deleted` included. |
| **Standards P2** the glossary's delete-flow entry said "the row is left exactly as it was" — the sentence ADR-0023 had just corrected — and its `_Avoid_: Removal` contradicts the machine names the same entry introduces | **fixed** | The wording is ADR-0023's ("left where the delete found it — except that a running turn has already been ended"), `Deleter::remove` is named as the task's machine name, "Removal" is dropped from the avoid list, and the `Stopped` entry admits the session a delete can leave behind. |
| **Standards P2** ADR-0013 still documented the refused batch that this change removes | **already fixed** | The "Superseded in part by ADR-0023" paragraph was written at `docs/adr/0013-event-log-single-writer-and-seq-counter.md:110-117` before the review returned; the reviewer's artifact predates it and the finding says as much. Verified present. |
| **Standards P2** ADR-0023's closing bullet ("the delete produces no `stopped` session") is contradicted by its own §2 | **fixed** | It now says a delete can leave a `stopped` row behind and that HS-020's eviction is still the first producer of a stop the operator did not ask for. |
| **Standards P3** the deleter's stream-close rationale claimed a benefit the shipped backends cannot reach, because the removal finishes inside the writer's 25 ms tick | **fixed** | Reworded: the ordering is for a backend whose teardown outlives that tick, and the comment says the frame is usually dropped with the stub and today's Docker backend rather than delivered. The test's comment already conceded the same thing. |
| **Standards P3** `apply_delete` was `apply_resume` with two words changed — a fourth variant of the same `StateError` → `ApiError` map | **fixed** | One `apply_trigger(state, id, trigger, refusal, operation)` with the four arms and their reasons documented once; `apply_resume` and `apply_delete` are gone. `apply_prompt` keeps its own loop, as the reviewer suggested, because its retry-once-on-idle is a different rule. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` are
green (212 lib tests, 22 integration binaries, 14 of them `tests/delete.rs`), `scripts/verify.sh` passes
with 35 cases and 150 assertions against the shipped artifact, and the demo (two backends plus the
forced path over a failing stub snapshot) was run against `dist/x86_64-unknown-linux-musl/holosmith`.
The three items the reviewer could not verify by reading — the gates, the harness's strict `"sessions":[]`
assertion and the Docker-backend demo step — are exactly what that run covers.
