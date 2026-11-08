# Review — PER-90 (HS-058, the Changes sheet)

Fixed point `6bdf543`; reviewed commits `39ae044` and `eacb926`. Two `reviewer` children (read-only), one per axis,
brief at `/tmp/holosmith/reviews/PER-90-review-brief.md`, diff artifact at `/tmp/holosmith/reviews/PER-90-diff.txt`.

---

## Standards axis (verbatim)

## Review

**Correct** (verified by reading, not assumed):
- The three fixed sentences are verbatim: `COMMIT_PROMPT` == ADR-0065 line 97; `CI_WARNING` == ADR-0066 line 185; `RESOLVE_PROMPT` is newly written where ADR-0064 D10 deferred it. No second spelling.
- Every `docs/notes.md` "Config and docs" deliverable is present: ADR-0067 added, `docs/stories/README.md` count 66→67 **and** the bullet, `docs/build.md` prose extended in the same paragraph style, a `### Landmines: the Changes sheet (HS-058)` in story order, `case_changes_screen` in `serve_cases` (build.md:496's rule).
- `GLOSSARY.md` gains one entry plus a `_Avoid_` line and updates "Changes view"'s machine names (`changes::Choices`) — matching the new struct. The `python3` SKIP matches fifteen existing precedents, so it is not the build.md P1 shape ("a skip for 'no daemon' is not that bug").
- Web idiom kept: no new dependency, no `innerHTML`, `textContent` only (main.ts:1326).

**Finding: P1 (hard violation) — ADR-0067 D2 says "Every sentence and every branch the sheet shows is a pure function in `web/src/transcript.ts` … `main.ts` keeps only the DOM"; `docs/notes.md` Web agrees ("a *sentence* is a rule too"). Three sentences and one branch live only in `main.ts`, untested:**
- `main.ts:1239` `files.append(make("li", "empty", "nothing committed on this branch yet"))`
- `main.ts:1485` / `main.ts:1488` the `done` strings passed into `sendPrompt` ("asked the agent to commit…", "asked the agent to resolve the conflicts…")

Nothing goes red if any of these is deleted (the bundle grep checks neither). Smallest fix: move the three sentences to `transcript.ts` (`emptyFilesLine`, `askedNotice(kind)`) with two tests. Related asymmetry: `COMMIT_LABEL`/`RESOLVE_LABEL`/`FORCED_DELETE_LABEL` are tested constants, but "Merge", "Publish", "Cancel", "Target", "Strategy", "Remote" (main.ts:1108/1119/1120/1164/1165/1168) are inline.

**Finding: P2 (judgement) — `GLOSSARY.md`:544 lists eight of the sheet's rules; ADR-0067 D2 lists fifteen** (adds `countsText`/`statusWord`, `targetOptions`/`preselectedTarget`, `publishQuestion`, `diffPath`, `sheetFailure`). Two spellings of one fact in the authority doc — add "among them" or the missing five.

**Finding: P2 (judgement) — needle uniqueness.** `scripts/verify.sh` greps `changes-conflict` and `changes-publish`, which are prefixes of siblings it also greps (`changes-conflict-text/-paths`, `changes-publish-start/-yes/-cancel`). Dropping the container id still passes, contra the landmine this story adds ("Pick needles that are unique in the bundle").

**Smells (baseline, all judgement):**
- *Duplicated Code* — `main.ts:1316-1330`, `1380-1398`, `1419-1431`, `1452-1466` are the same `envelopeOf(text)` → `envelope === null ? sheetFailure(status, text) : sheetFailure(code, message)` → `say`. One helper would do; `getJson` already exists.
- *Speculative Generality* — `STATUS_WORDS` R/C/U ("The rest are here because a future flag could emit them", transcript.ts) and `targetOptions`' fallback for "a shape the server does not send".
- *Data Clumps* — `Changes::read(worktree, base, targets, remotes)` threads two config lists separately then rebuilds `Choices`; the test helper grows `read_with`/`read_with_remotes` for it. Taking `&Choices` would prevent mismatched pairs.

**Merge verdict: OK with notes** (fix the P1 before ship; it is three small moves).

## Spec axis (verbatim)

## Review — Spec axis (PER-90)

**(c) Implemented, but wrong**

**P1 — the conflict state is erased by a stale-but-clean read.** Acceptance: *"Conflict state shows the paths and the 'Resolve with agent' action."* `render()` wipes the `409`'s facts whenever the *older* read said clean — `web/src/main.ts:1264-1267` (`if (answer.mergeable === true) { conflictMessage = null; conflicts = null; }`) — and `render()` runs on every `session.status` frame (`update`, main.ts:1576) and in `sendMerge`'s `finally` (main.ts:~1400), where no re-read follows a failure. So read-says-`mergeable:true` → tap → `409 conflict` leaves `said`/`paths` null: `conflict.hidden = true`, no paths, Resolve hidden, and only the fixed envelope sentence (`crates/holosmith/src/http/error.rs:505`, which names no path; the paths live only in `conflicts`, error.rs:507). This contradicts ADR-0067 D7 ("the area shows whichever sentence is newer"). Smallest fix: clear inside `load()` when a *new* answer lands, not in `render()`. **Goes red if left: nothing** — the precedence lives in `main.ts`, so `npm test` cannot see it, which is also the counter-example to D2's "every branch the sheet shows is a pure function".

**(a) Missing or partial**

**P1 — the CI warning is never in the publish confirm step.** Spec item 12 (`HolosmithSpec.md:232`): *"`touches_ci` is true … and the UI shows a warning before confirm"*; ADR-0067 D8: *"shown in two places — beside the counts, and inside the confirm step before the second tap."* `confirm.append(question, remoteField, yes, no)` (main.ts:1171) has no warning; `publishQuestion` (transcript.ts:826) has no CI text; the single warning node (main.ts:1095) sits above the file list (main.ts:1177). The comments at main.ts:1093-1094 and 1148-1149 assert the opposite of the code. **Goes red if it stays missing: nothing** (the gate greps the string once, and it is found beside the counts).

**P2 — "preselecting … `ff-only`" is unmeasured.** Implemented at main.ts:1118 (`strategy.value = STRATEGIES[0]`), but nothing asserts `STRATEGIES` (transcript.ts:766) is `["ff-only","merge","squash"]`; `web/test/transcript.test.ts` uses `"ff-only"` only as a literal in `mergedNotice`, and `case_changes_screen` greps only `changes-strategy`. Reorder the constant and **nothing goes red**, while every merge becomes a non-default strategy.

**(b) Scope creep: none.** The only server change is the one additive key (`changes.rs:150-158`, `:200-207`, `http/changes.rs:89-93`); `existing_targets` (`changes.rs:785`) is the same walk grown a return value, and `config/mod.rs:665`'s `is_ref_name` means the longer walk adds no new refusal.

**Merge verdict: BLOCK** (two P1s).

---

## Resolution

| # | Finding | Verdict | What changed |
| --- | --- | --- | --- |
| 1 | Standards **P1** — three sentences and one branch live only in `main.ts`, untested (the empty-list line and the two "asked…" notices) | **Fixed** | `transcript.ts` gained `NOTHING_COMMITTED`, `COMMIT_ASKED` and `RESOLVE_ASKED`, each with a test (`the sheet's two prompt notices and its empty list are sentences with tests`) — and the two notices assert their own words ("commit"/"conflicts", both naming the retry) so a swap goes red. The button *labels* stay inline: `deleteSection` has always kept its affordance's button text in `main.ts` ("Delete session", "Delete", "Cancel", the confirm question), so the split the ADR claims is "every **sentence the server's answer produces** is a rule"; D2 now says so in as many words rather than claiming the labels too. |
| 2 | Spec **P1** — the CI warning is never inside the publish confirm step; the ADR claimed it was | **Fixed** (the reviewer's smallest fix, and the ADR corrected) | `publishQuestion(branch, remote, touchesCi)` now takes the flag and appends `CI_WARNING` to the confirm paragraph, and the sheet passes `answer.touches_ci` in both places it draws the paragraph (opening the step, and a remote change). New test `the CI warning is inside the publish confirm, not only above the list` asserts both arms, so deleting the third argument goes red. The stand-alone warning line beside the counts stays: item 12 asks for a warning *and* for one before confirm, and ADR-0067 D8 now describes the pair the code actually has. |
| 3 | Spec **P1** — a `409 conflict` is wiped by the stale `mergeable: true` read when the next `render()` runs | **Fixed** | The clearing moved out of `render()` into `load()`: only a **new answer** can retract a refusal's words. `render()`'s comment now says why (a render also happens on a state frame and in a request's `finally`, where the answer predates the tap) and ADR-0067 D7 carries the correction. The merge's failure path also re-reads (`await load()` after the refusal), so a `409 dirty_worktree`/`session_running` makes the disabled reason converge on what the refusal named instead of leaving a button that would refuse again. |
| 4 | Spec **P2** — the `ff-only` preselection is unmeasured; reordering `STRATEGIES` would go red nowhere | **Fixed** | `the strategy picker defaults to ff-only, and its words are the contract's` pins `STRATEGIES` as `["ff-only", "merge", "squash"]` with `ff-only` first, so a reorder fails a test rather than silently making every merge a `merge` commit. |
| 5 | Standards **P2** — `GLOSSARY.md` lists eight of the sheet's rules while ADR-0067 D2 lists fifteen | **Fixed** | The entry now says "among them" and names `publishQuestion` too, so the two documents cannot drift apart about a list neither owns. |
| 6 | Standards **P2** — the gate's id needles are prefixes of their siblings' (`changes-conflict` matches `changes-conflict-text`), so deleting a container element would stay green | **Fixed** | The needles are the assignments' own quotes — `id="changes-conflict"`, eighteen more in the same shape — which no sibling's id contains; the list grew the three ids the review's own walk found missing (`changes-conflict-text`, `changes-conflict-paths`, `changes-question`, `changes-remote`). The landmine this story added now records the first draft's mistake, and the case still counts every needle that found something. |
| 7 | Standards smell — *Duplicated Code*: the same `envelopeOf` → `sheetFailure` → `say` block four times | **Fixed** | One `refusalLine(status, text)` next to `envelopeOf`, named by the three lines it replaces ("an envelope, or the plain text a route that failed left behind"), and the four call sites are one line each. `getJson` was not reused: it parses a JSON body itself, and the diff route answers `text/plain`. |
| 8 | Standards smell — *Speculative Generality*: `STATUS_WORDS`' R/C/U, and `targetOptions`' fallback for "a shape the server does not send" | **Fixed** for the first, **dismissed** for the second | The map now holds exactly the four letters `--no-renames` can emit (A, M, D, T); the fallback still shows any other letter as itself, which is the honest answer and is tested (`statusWord("R") === "R"`). The `targetOptions` fallback is kept **with a citation**: `deletedNotice` and `budgetNotice` already carry a shape "the producer does not send" — a `kept` with neither half, a spend nothing reported — because a client that renders nothing for an unremarkable shape is worse than one that renders the one it measured; both arms are tested. |
| 9 | Standards smell — *Data Clumps*: `Changes::read(…, targets, remotes)` threads two config lists and rebuilds `Choices` | **Dismissed**, with citation | ADR-0061's and ADR-0063's rule is that `Changes::read` is **handed the config's lists and measures the answer itself** — `target` is a measurement of the repository, not a value the caller supplies. A `&Choices` parameter would be a pre-derived answer wearing the output type's name (its `targets` are already filtered, its `remotes` are not), which is exactly the second spelling those ADRs forbid. The two lists come from two config keys (`merge.targets`, `publish.remotes`) and the call site reads both from `AppState`'s config in two lines; the test fixture's `read_with`/`read_with_remotes` are the *test's* two spellings of one call, not the module's. |

Nothing was dismissed without a citation. The three findings that changed behaviour (1, 2, 3) were re-measured
after the fix — `npm test` went 69 → 72, `npx tsc --noEmit` clean, and the full gate re-ran green
(**84 cases, 1414 assertions**, no `SKIP`), including `case_changes_screen` with the strengthened needles. One
further fix came out of reading the demo's own output: the warning was glued to the ask ("…nothing is forced this
branch changes CI configuration…"), so it is its own line now (`white-space: pre-line` on the confirm paragraph) and
the test compares the two lines instead of a substring; the gate was re-run after that (84 cases, 1414 assertions),
and the demo on the rebuilt artifact shows the sentence on its own line in the confirm step.
