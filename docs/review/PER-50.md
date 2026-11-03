# Review — PER-50 (HS-018, the stopped state and `POST /resume`)

- **Fixed point:** `8ff1d77`
- **Reviewed change:** `dce53c4` (`8ff1d77..HEAD`)
- **Reviewer:** a fresh `reviewer` subagent running `/home/adamolsson/.agents/skills/code-review/SKILL.md`,
  read-only, no shell. Its claims were checked by the orchestrator; the Resolution table records what was
  fixed and what was dismissed.
- **Verdicts:** Standards — OK with notes (three cheap doc contradictions); Spec — OK with notes (a dead
  abort-retry path, an incorrect ADR timetable, and two deferrals worth recording).

This review is saved verbatim. The `## Resolution` section at the end was appended afterwards.

## Review method

Read: the brief, the code-review skill, the full artifact `/tmp/holosmith/reviews/PER-50-diff.txt` (`8ff1d77..HEAD`, `dce53c4`, 15 files), and the touched files in the tree — `crates/holosmith/src/http/{app,sessions,error}.rs`, `crates/holosmith/src/sessions/{state,repository,activity}.rs`, `crates/holosmith/src/{provisioner,ladder,docker,stub}.rs`-by-grep, `crates/holosmith/tests/{resume.rs,support/mod.rs,turns.rs,steer.rs,abort.rs}`, `web/src/{main,transcript}.ts`, `web/src/style.css`, `web/test/transcript.test.ts`, `tests/events_malformed.rs`, `scripts/verify.sh` (the `serve_cases` array), `GLOSSARY.md`, `HolosmithSpec.md` items 8/9/14/15/16 + the API table, `docs/stories/phase-1-session-lifecycle-stub.md` HS-018/019/020, `docs/adr/0018/0021/0022`, `docs/build.md`. ADR-0022 was treated as a claim to check. No files modified, nothing run.

---

## Standards

**Correct**
- Route seam is the established one, not a second one: `.post("/api/sessions/{id}/resume", move |id: Result<Path<String>, PathRejection>| … unwrap_or_default())` (`http/app.rs:434-445`) exactly as the siblings, so a `%FF` segment stays inside ADR-0008's envelope; the router's doc list was updated in the same hunk (`app.rs:338-340`).
- Lock discipline holds. `live_at_cap` (`sessions.rs:635-646`) and `one_locked` (`:394-401`) are fully synchronous; the `Resume` insert and status write are inside `Transitioner::apply`, so no guard lives across the `apply_resume` await. The handler writes no status itself (so `no_status_write_outside_the_one_function` still holds), emits no event itself (so broadcast-after-COMMIT is untouched), and never touches `sessions.last_seq`.
- New prose follows house style — code-vs-sentence (`NOT_RESUMABLE`, `:53-56`), the "table is the authority" doc on `apply_resume` (`:607-613`), and the ADR's decision format.
- `tests/resume.rs` walks that prove they found something: `manifest().iter().find(…).expect("the resume is registered")` (`:404-408`), `assert!(!before.is_empty(), …)` (`:178-180`), and the cap test's `row(...).status == Status::Stopped` (`:363-365`).
- No glossary drift on the term this story defines: **Stopped** (`GLOSSARY.md:144-146`) matches the table (`sessions/state.rs:205-213`).

**S1 · should-fix (P1) — the diff adds a second `**Resume**` entry instead of editing the one that exists.**
```diff
+**Resume**:
+Bringing a stopped (or failed) session back to `idle` under the same id: … _Avoid_: Restart, reopen, revive, unpause
+
 **Resume**:
 Bringing a stopped session back with the same id: the `stopped` + `resume` row … _Avoid_: Restart, reopen, wake
```
`GLOSSARY.md:144-154`. Two entries, one term, different scope (the second omits `error`) and different `_Avoid_` lists; the second is now unreachable prose a reader has no rule to choose between. The glossary is the vocabulary authority (`docs/stories/README.md:12`) and the convention is one entry per term — the same defect a prior review raised and the author fixed for **Event writer** (`docs/review/PER-42.md:26`). *Smallest fix:* delete lines 152-154, keep the new, fuller entry.

**S2 · should-fix (P1) — `**Composer**` still describes three actions and says `stopped`/`error` offer none, which this diff makes false.**
```markdown
The session screen's box of guidance and the three actions it offers: prompt, steer and abort. Which of the three exist is the state's (`affordances`): prompt on `idle`, steer and abort on `running`, none on the four other states.
```
`GLOSSARY.md:85-87`, contradicted by `web/src/transcript.ts:286-296` (`resume: state === "stopped" || state === "error"`) and `web/src/main.ts:562` (the fourth button). The diff edits this very section of this very file (`GLOSSARY.md:144-154`), so it is in scope. *Smallest fix:* "the four actions it offers: prompt, steer, abort and resume … prompt on `idle`, steer and abort on `running`, resume on `stopped` and `error`, none on `starting` and `deleting`."

**S3 · should-fix (P1) — two neighbouring ADRs now assert facts this diff falsifies, without the `**Superseded by ADR-00NN:**` annotation the files already use.**
```markdown
The composer exists in every state and its three actions come from `affordances(state)`: prompt on `idle` … steer and abort on `running`, nothing on `starting`, `stopped`, `error` or `deleting`.
```
`docs/adr/0018-…md:86-88` — false for `stopped`/`error` since this commit, and the same file demonstrates the convention two lines later ("**Superseded by ADR-0019:** … **ADR-0020:** … **ADR-0021:**", `:90-92`). Likewise `docs/adr/0021-…md:86-87`: "`resume` is now the only trait method without one, **which is HS-018's**" — HS-018 deliberately leaves it uncalled, and ADR-0022 records that. *Smallest fix:* one bracketed sentence in each (`**Superseded by ADR-0022:** a fourth action, Resume, on `stopped` and `error`; `SessionBackend::resume` stays uncalled`).

**S4 · consider (P2, Duplicated Code) — the "resume, then provision" routine now exists twice, and the older copy's doc points at the new one as if it would share it.**
`ladder.rs:233-264` (`apply(Resume)` → tolerate `Unreported` → refetch the row → `provisioner().spawn`) and `http/sessions.rs:592-606` (same sequence with a different refetch). The ladder's doc still says "which is the same path HS-018's endpoint **will take**" (`ladder.rs:234-236`) — the endpoint took it by copying. *Smallest fix:* one `async fn resume_and_provision(&self, id: &str)` on the handles both hold (`Arc<Mutex<Database>>` + `EventWriter` + `Provisioner`), called twice.

**S5 · nit — the `drive` test helper is now a third copy.**
`tests/resume.rs:144-149`, byte-identical to `tests/turns.rs:126-130` and `tests/steer.rs:151-155`, while `tests/support/mod.rs` is the module all three already import. *Smallest fix:* move `drive` into `support` and delete the three bodies.

**S6 · nit — `the_resume_route_is_recorded_mutating` spends most of its body on things it does not own.**
`tests/resume.rs:410-418` re-pins `SESSION_STOPPED`/`SESSION_IDLE`/`SESSION_RUNNING`/`SESSION_STARTING`/`CAP_REACHED` strings already asserted in `src/http/error.rs:477-478` and `:438`, then `assert!(matches!(state().backend, Backends::Stub(_)))` with the comment "so the test above can assert on it" — no test above reads that handle. *Smallest fix:* keep the route/method/kind assertions, drop the rest.

**S7 · nit — `live_at_cap` repeats `create_locked`'s cap predicate.** `sessions.rs:635-646` vs `:291-297`. Judgement call, report only: ADR-0022's "two statements in two scopes" is the reason it cannot be one function, and the shared part is two lines.

**S8 · nit — `docs/build.md:140-144` was reflowed around pre-existing ragged lines, leaving `A case that must` hanging at the end of a long line followed by the old two-word lines.** Cosmetic; re-wrap the paragraph.

---

## Spec

**Correct**
- Bullet 2 is proven end to end: 202 with `{"status":"starting"}` (`tests/resume.rs:182-190`), then `starting → idle` driven by the provisioner, asserted from both the row and the log (`:196-198`, `:230-241`).
- Bullet 5 is proven the way the brief demanded — non-vacuously. `before` is captured only after the settle event is committed and is asserted non-empty (`:171-180`); `after.len() > before.len()` (`:220-225`); then the slice comparison (`:226`). It would fail on any rejection of a row, and it would also fail if `read_since` returned descending order, so it is not a tautology.
- Bullet 3's API half is proven (`:381-397`: `409 session_stopped` for prompt, steer and abort, with the envelope's content type), and the refusal code comes from the one table in `http/error.rs:264-271`.
- Bullet 4's "or returns 409" branch is coherent: checked before the transition, `cap_reached` (the create's own code), and the cap test proves the row untouched (`:363-365` — status and `stopped_at` both still the stopped values). D3's ordering argument in ADR-0022 is sound.
- D6 verifies: `SessionBackend::resume` still has no production caller (only `backend/mod.rs:311-315` dispatch and the two impls); the resume goes through `Provisioner` (`sessions.rs:606`).
- Scope did not grow: no eviction, no idle stop, no delete, no retention, no Docker. `min_free_gb`/507 for resume is correctly not here — it is a phase-2 story (`docs/stories/phase-2-deployable-mvp.md:255-260`).
- The UI retry helper cannot loop, cannot retry twice, and cannot mask a second refusal: the recursion passes `retried=true` (`main.ts:631`) which short-circuits at `if (!mayResume || retried || !RESUMABLE.has(envelope.code))` (`:614-620`), the bound is 40 × 250 ms = the ADR's ten seconds (`:581-593`), and `resumeSession` passes `mayResume=false` (`:696`).
- D5's matrix is what ships and is tested: `web/test/transcript.test.ts:282-292`.

**SP1 · consider (P2) — the "resume and retry" rule is applied to abort, where the retry is dead by construction and the resume does the opposite of what the operator asked.**
`abortTurn` sends `post("/abort")` (`main.ts:671`). A `409 session_stopped` refusal here can only happen in the stale-badge race (the button is hidden outside `running`, `:768`), and the helper then resolves it as `RESUMABLE` → resume → `waitIdle` → retry. After a resume the row is `idle`, and abort requires `running` (`sessions.rs:481-484`), so the retried abort always answers `409 session_idle` and `abortTurn` says `session_idle: the session is not running, so it has no turn to abort` (`:674-676`). Net effect of pressing Abort in that race: the session is provisioned (a live slot, worktree, container) and then an error is shown. The spec does say "Prompt, steer and abort … the UI resumes and retries" (`HolosmithSpec.md:155`), so this is literally compliant; ADR-0022's "every composer action is a POST through the same function" does not notice that one of the three cannot profit from it. *Smallest fix:* pass `mayResume=false` for abort (the mechanism already exists), or record the second refusal in ADR-0022 as the intended answer.

**SP2 · consider (P2) — the demo cannot be performed against the product, and the stopped row's *reason* is never asserted; the ADR's "next one" claim is wrong.**
- The demo is "stop a session, see it `stopped` with a reason" (`docs/stories/phase-1-session-lifecycle-stub.md:200`); the tests stop through `drive(&state, &id, Trigger::IdleStop)` with `reason: None` (`tests/resume.rs:144-149`, `:162`), so nothing in the story ever asserts a `status_reason` on a `stopped` row. The "never a dead end" half of bullet 1 is proven; the "with a reason" half is not.
- The deferral itself is defensible (the table's `evict`/`idle_stop`/`drain` rows are HS-012's and already exist; inventing `POST …/stop` would add vocabulary the spec does not have) — but ADR-0022 prints an incorrect timetable: "The **`stopped` producer remains open** … Both stories are blocked by HS-018, so the gap closes **with the next one**" (`docs/adr/0022-…md:78-80`). The next story is HS-019 delete, blocked by HS-018 (`docs/stories/phase-1-session-lifecycle-stub.md:216`), and produces no `stopped`; the first producer is HS-020, blocked by HS-019 (`:232-233`). *Smallest fix:* "the gap closes with HS-020" and one `assert!(stopped.status_reason.is_some())` is impossible here — instead say plainly that the reason half of the demo is HS-016/020/021's to demonstrate.

**SP3 · consider (P2) — spec item 8's `error` row is conditional and the condition is not implemented or tested.**
`| error | resume (clone present) | starting |` (`HolosmithSpec.md:151`) against an unconditional row (`sessions/state.rs:224-228`) and a handler that checks only the status (`sessions.rs:584-586`). The only error-resume test fails the stub at `Step::Network` (`tests/resume.rs:265-269`), i.e. precisely the case where the clone is present; a resume of a session that died at `Step::Worktree` is neither refused nor covered. *Smallest fix:* report it into HS-024's brief rather than implementing a worktree probe here, and say so in ADR-0022 (which currently only defers `SessionBackend::resume`).

**SP4 · consider (P2) — the reason-clearing half of a resume is asserted only where the reason was already null.**
`assert_eq!(resumed.status_reason, None);` (`tests/resume.rs:200`) runs on the `idle_stop` path, where `drive` passed `reason: None`, so a regression that left a stale reason on the `starting` row would pass every test. The `error` test asserts only `body["status"]` (`:283-285`). *Smallest fix:* in the error test, assert `body["status_reason"]` is `Value::Null` on the 202 — the write is correct (`repository.rs:34`, `UPDATE_STATUS` sets `status_reason = ?3` with `None`), the test just does not pin it.

**SP5 · consider (P2, latent) — the resume re-runs the worktree step, and the provisioner's failure path undoes it.**
`Provisioner::run` walks `Step::ALL` for whatever session it is handed (`provisioner.rs:75-99`) and on a later failure calls `undo(Worktree)` (`:104-120`), which for a session whose worktree the stop preserved is the one resource bullet 5 and item 9 promise to keep ("History, worktree, events and images are kept", `HolosmithSpec.md:162`). Unreachable today — the docker backend's `provision`/`undo` are stubs (`docker.rs:32-44`) — and ADR-0022 explicitly defers "whether a resumed session re-runs every step or only the container's" to HS-024. Reporting so it is not lost, since the deferral covers `SessionBackend::resume` but not the undo risk.

**SP6 · consider (P2) — a just-resumed session is HS-020's first eviction candidate.**
`UPDATE_STATUS` deliberately leaves `last_active_at` alone ("it is activity, and HS-020/HS-021 own when it moves", `repository.rs:31-34`), and nothing in the resume touches it (`sessions.rs:592-611`), while eviction reads ascending `last_active_at` (`repository.rs:46-49`). A session stopped for a week and just resumed therefore sorts as the least recently active `idle` row. Deliberate or not, no ADR records the choice. *Smallest fix:* one `activity::touch` beside `apply_resume`, or one clause in ADR-0022.

**SP7 · nit — `resumeSession` discards `waitIdle`'s result.** `await waitIdle();` (`main.ts:706`) vs the retry path's `if (!(await waitIdle()))` (`:628`): a session that never comes back leaves the last polled notice ("nothing can be sent until it finishes provisioning") with no timeout message.

**SP8 · nit — the retried prompt is the text from *before* a wait of up to ten seconds.** The body is captured at `main.ts:727`, `waitIdle` may run ten seconds, and on success `guidance.value = ""` (`:733`) wipes anything typed in between — the operator's newer text is lost and the older text is what was sent.

**SP9 · nit — `post` fabricates an envelope whose code is not ADR-0008 vocabulary.** `{ code: "unreachable", message: "this session did not come back" }` (`main.ts:629`); `unreachable` is the client's own log prefix in the `catch` blocks, never a server code (`http/error.rs:15-73`), so a caller matching on codes matches a word no API answer can carry.

---

## Verdicts

- **Standards: OK with notes.** No blockers. S1, S2 and S3 are three cheap documentation fixes (a two-line deletion plus two clauses) that should land before merge because they leave the vocabulary authority self-contradictory; S4-S8 are report-only.
- **Spec: OK with notes.** Every acceptance bullet has a test behind it, and the ones the brief worried about hold up (the prefix assertion is non-vacuous, the cap test pins the untouched row, the retry helper cannot loop, double-retry or swallow a second refusal). The worst issue within this axis is SP1 (a provably dead retry that provisions a session on an Abort tap) followed by SP2's incorrect "the gap closes with the next one" claim.
- **Merge verdict: OK with notes** — fix S1/S2/S3 and, if it is judged cheap, SP1 and SP2's ADR sentence.

## Commands a supervisor must run (this role has no shell)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                     # +5 in the new `resume` integration binary (one is a non-async #[test])
cd web && npm test             # expect 18 transcript tests (17 + the affordance matrix split)
scripts/verify.sh              # expect 32 serve cases (+ the CORS sweep)
```
`scripts/verify.sh`'s two new cases depend on `$created_id` still being `idle` when they run (`verify.sh:288-291`), which holds only because `case_steer` polls until `idle` (`docs/build.md:131-137`) — if the case order ever changes, `case_resume_on_an_idle_session` will assert the wrong state, so the run is worth watching.
## Resolution

| # | Axis | Finding | Verdict |
| --- | --- | --- | --- |
| S1 | Standards, should-fix | The diff added a **second** `**Resume**` glossary entry beside the one that already existed, with a different scope and `_Avoid_` list | **Fixed.** One entry, the fuller one, with both avoid-lists merged (`Restart, reopen, revive, unpause, wake`) |
| S2 | Standards, should-fix | `**Composer**` still described three actions and said `stopped`/`error` offer none | **Fixed.** Four actions, with `resume` on `stopped`/`error` and none on `starting`/`deleting` |
| S3 | Standards, should-fix | ADR-0018's composer sentence and ADR-0021's "`resume` is now the only trait method without one, which is HS-018's" were both made false by this story, without the superseding annotation the files use | **Fixed.** ADR-0018 gains "**Superseded by ADR-0022:** a fourth action, Resume…", ADR-0021 gains "**Superseded by ADR-0022:** HS-018 deliberately leaves `resume` uncalled" |
| S4 | Standards, consider | "Resume then provision" now exists twice (the ladder's rung 2 and the endpoint), and the ladder's doc still said the endpoint "will take" that path | **Dismissed with a note.** The two are not the same routine: the ladder's is `apply(resume)` then spawn, deliberately best-effort and inside a task that must not fail; the handler's is a request with a cap check, a client-visible refusal and a reason-clearing write. Factoring them into one `resume_and_provision` would hide that difference behind a flag — and the ladder's doc line is now stale, so it was corrected instead |
| S5 | Standards, nit | The `drive` test helper is a third copy | **Dismissed.** After the SP2/SP4 fix this story's helper takes a `reason` the other two do not; they are no longer byte-identical, and moving the widened one into `support` would push a parameter two tests never use |
| S6 | Standards, nit | `the_resume_route_is_recorded_mutating` re-pinned five codes and asserted a stub handle nothing reads | **Fixed.** Route, method and kind only |
| S7 | Standards, nit | `live_at_cap` repeats the create's cap predicate | **Dismissed as reported.** The create's count shares one lock scope with its insert and the resume's cannot (ADR-0022), and the shared part is two lines |
| S8 | Standards, nit | `docs/build.md` was reflowed around ragged lines | **Fixed.** The paragraph is re-wrapped |
| SP1 | Spec, consider (P2) | The resume-and-retry rule was applied to abort, where the retry is dead by construction: a resume ends `idle`, abort needs `running`, so the retried abort can only answer `session_idle` — after a provisioning run nobody asked for | **Fixed.** `abortTurn` passes `mayResume: false`, and ADR-0022 states why the spec's "the UI resumes and retries" is written for the requests a resume can make legal |
| SP2 | Spec, consider (P2) | The demo's "stopped **with a reason**" was never asserted (the test's stop passed `reason: None`), and ADR-0022 claimed the gap "closes with the next one" — HS-019 is next and produces no `stopped` | **Fixed.** The test now stops with `Some("idle_stop")`, asserts it on the row and asserts the resume clears it; ADR-0022 names HS-020 as the first producer and HS-021 as the second |
| SP3 | Spec, consider (P2) | Spec item 8's `error` resume row is conditional ("clone present") and the condition is neither implemented nor recorded | **Recorded, not implemented.** Implementing a worktree probe needs a real worktree, which is HS-024's; ADR-0022 now says so in a section of its own, and HS-024's brief carries it |
| SP4 | Spec, consider (P2) | The reason-clearing half of a resume was asserted only on a row whose reason was already null | **Fixed** by the SP2 change (the stop's reason is non-null and is asserted cleared), and the `error` test now asserts `status_reason` is `null` on the 202 |
| SP5 | Spec, consider (P2, latent) | A resumed session re-runs the `worktree` step, and the provisioner's failure path undoes what completed — for a session whose worktree the stop preserved | **Recorded, not implemented.** Unreachable until Docker fills the trait in; ADR-0022 carries it as a named risk for HS-024 |
| SP6 | Spec, consider (P2) | A just-resumed session sorted as HS-020's least recently active idle row, because nothing moved `last_active_at` | **Fixed.** The handler touches the row, as a prompt and a steer do — a resume *is* the operator acting on the session |
| SP7 | Spec, nit | `resumeSession` discarded `waitIdle`'s answer | **Fixed.** It now says the resume was accepted but the session has not come back |
| SP8 | Spec, nit | A retried prompt sent text captured up to ten seconds earlier and then emptied the box, losing anything typed in between | **Fixed.** The sent text is captured, and the box is cleared only if it still holds it — for the prompt and the steer |
| SP9 | Spec, nit | `post` fabricated an envelope whose code (`unreachable`) no API answer carries | **Fixed.** It throws, which the callers already report as `unreachable:` in their own words |

Gates re-run after the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` (209 lib + 228 across twenty-one integration binaries), `cd web && npm test` (18),
`scripts/verify.sh` (32 cases + the CORS sweep, 134 assertions).
