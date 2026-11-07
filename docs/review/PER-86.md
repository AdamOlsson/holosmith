# PER-86 — HS-054, the merge's fast-forward path

Review of `2036003...HEAD` (commits `d493e1a`, `a1df376`, `bf61205`), run by a fresh `reviewer` child against
`/tmp/holosmith/reviews/PER-86-diff.txt`, with the spec axis taken from the Linear body, the story slice and
spec item 12's rules 2 and 4. Saved verbatim, with one formatting change (its duplicate `## Review` heading removed); the `## Resolution` table below is the only edit.

## Review

I read the 2 486-line diff artifact in full plus the files it names, and verified each claim against the working tree (`merge.rs`, `http/merge.rs`, `http/changes.rs`, `http/error.rs`, `worktree.rs`, `tests/merge.rs`, ADR-0063, `scripts/verify.sh`, `docs/notes.md`, `GLOSSARY.md`). **Note:** `watchdog_diff` was not used — this is a committed-range review (`2036003..HEAD`), so the artifact was the only source and I did not run `cargo` or `git`.

---

## Standards axis

**Verdict: OK with notes (no P0; two P1s, both small, should land before HS-055 builds on them).**

What holds, verified: the two update paths are genuinely *distinguished* by tests, not just present — `tests/merge.rs:186-243` asserts different things move (`repo/a.txt` on disk and a clean `status --porcelain` vs. `HEAD` still at base and no file), and an implementation that always ran one arm would go red on the other, which is the failure mode `docs/notes.md:1794` ("a symmetric fixture cannot measure a direction") exists for. No database write anywhere in the handler or the module (`http/merge.rs:103-110` is a `get`; `merge.rs` never names the database), so D7's row claim holds. `registrations` → `checkouts` is a true refactor (one parser, same order, same `worktree ` prefix filter; `worktree.rs:576-602`). The hook test and the unrelated-dirty test are both load-bearing by construction.

---

**Finding: P1 — the one test ADR-0063 D7 cites for "what a merge tells the changes view" cannot fail.**
- **Location:** `crates/holosmith/tests/merge.rs:704-739` (`the_sessions_changes_view_answers_ahead_zero_after_a_merge`).
- **What & Why:** every post-merge value asserted equals its pre-merge value: `ahead` 1 → 1 (`:721` and `:732`), `behind` 0 → 0 (`:722` and `:736`), `base_commit` unchanged (`:733`), `mergeable` `true` (`:737`, and it was `true` before too). A build in which the merge wrote nothing to the repository passes all of them; the only assertion the merge can turn red is `:725` (`200`). The fixture *cannot* be made to contrast: for an `ff-only` merge the target must be an ancestor of the branch, so `behind` is necessarily `0` **before** the merge as well, and `mergeable` is true both sides — the claim in the test's own comment (`:707-708`, "what changes is `behind` (to 0) and `mergeable`") is unfalsifiable. The name also asserts the opposite of the body (`…answers_ahead_zero…` while `:732` requires `1`), so a grep for the test argues for the wrong behaviour.
- **Suggested fix shape:** make the test prove what it can prove — rename it to say the summary is unchanged, and supply the contrast from the repository instead of the JSON: keep `ahead == 1` on both sides, drop `behind`/`mergeable`, and add `assert_eq!(fixture.rev("refs/heads/main"), tip)` plus `assert_eq!(fixture.rev(&fixture.branch()), tip)`. That is non-vacuous (it fails if the merge wrote nothing) and it states the real contract: the summary is a measurement of the branch, the merge moved the target.

---

**Finding: P1 — the merge's diagnostics are dead code, and its git failures are logged as the changes view's.**
- **Location:** `crates/holosmith/src/merge.rs:186-188` (`MergeError::sentence`), `crates/holosmith/src/http/merge.rs:173-188` (`refusal_of`), against `crates/holosmith/src/http/changes.rs:254-271`.
- **What & Why:** `refusal_of` is a bare `match` that maps to `ApiError`s and **logs nothing**, yet its own doc at `http/merge.rs:176` says "The path the changes view's sentences cannot carry goes to the log here too". `MergeError::sentence()` is the only thing that formats those paths (`TargetDirty`'s checkout path at `merge.rs:204-206`, `CheckedOutElsewhere`'s at `:207-210`, `Unlisted`'s configured list at `:191-196`) and it has **no caller** in `src/` or `tests/` — `pub fn` on a `pub enum` in a `pub mod`, so nothing even warns. So the operator never learns which checkout held the target or which targets are configured; the precedent in the neighbouring route is explicit (`http/changes.rs:161-171` logs `sentence = other.sentence()` before answering). Separately, `MergeError::Changes(ChangesError::Git(..))` — the arm `update_ref` (`merge.rs:386-390`) and `fast_forward` (`:427-433`, `:450-452`) build when a merge **write** fails — is answered by `http::changes::refusal`, which logs `target: "holosmith::http::changes"` and the message "the changes view could not read the repository" with `operation = "changes"` (`http/changes.rs:264-271`). A merge that failed to write a ref is thus logged as a read that failed, in another module.
- **Suggested fix shape:** one line at the head of `refusal_of` — `tracing::warn!(target: "holosmith::http::merge", sentence = error.sentence(), "the merge was refused")` — which both implements the doc and gives `MergeError::sentence()` its caller; and for the `Changes(Git)` arm, log under this route's target/operation before delegating (or pass the operation word in, the way every other caller of `internal` does).

---

**Finding: P1 — two new module docs state a fact the code contradicts.**
- **Location:** `crates/holosmith/src/merge.rs:34-36` and `crates/holosmith/src/http/merge.rs:23-25`.
- **What & Why:** both say "`GET …/changes` answers `ahead: 0` afterwards because that is a measurement of the branch". A fast-forward moves the *target* to the branch tip and does not touch the branch, so `ahead` (`rev-list --count <base>..agent/<id>`) keeps its value — `1` in this change's own fixture (`tests/merge.rs:721` and `:732`). The same sentence appears in ADR-0063 D7 and in the handoff it was copied from; reported on the spec axis below, since the ADR is the contract. Two module docs asserting the opposite of the suite is exactly what a later reader will believe instead of the test.
- **Suggested fix shape:** "…and `GET …/changes` answers the same `ahead` it did before, because `ahead` is a measurement of the branch and a fast-forward moves only the target."

---

**Finding: P2 — the invented `409 target_checked_out` is the only refusal with no assertion at the status/code level.**
- **Location:** `crates/holosmith/tests/merge.rs:369-410` (module-level only), `crates/holosmith/src/http/error.rs:433`, `crates/holosmith/src/http/merge.rs:185`.
- **What & Why:** `a_target_checked_out_in_another_worktree_is_refused` matches on the enum variant and never sees an HTTP answer, so `ApiError::target_checked_out`'s constant and `refusal_of`'s arm for it are pinned by nothing; `case_merge` (`scripts/verify.sh:5452-5665`) never reaches that state either (it only walks unchecked-out, checked-out, `target_dirty`, `not_fast_forward`, and four `422`s). Meanwhile `tests/merge.rs:10-11` claims the wire group proves "the six refusals as status **and** code" — it proves five (the sixth being this one). Minor: `the_four_bad_requests_move_no_ref` (`:554`) carries seven request/code pairs.
- **Suggested fix shape:** add one wire test reusing `a_target_checked_out_in_another_worktree_is_refused`'s fixture (`git checkout -b elsewhere` then `git worktree add other main`) asserting `409` + `"target_checked_out"`; or change `tests/merge.rs:10-11` to "five" and say where the third arm is measured.

---

**Finding: P2 — a shared refusal now speaks about a review on a route that was asked to merge.**
- **Location:** `crates/holosmith/src/http/changes.rs:42`, `:56`, `:60`, `:64`, `:67`; used by `crates/holosmith/src/http/merge.rs:119` and `:180`.
- **What & Why:** all five `409 no_worktree` sentences end in a clause about the *read*: "so there is nothing to review" (`:42`, `:56`, `:60`) and "so there is nothing to compare" (`:64`, `:67`). On `POST …/merge` the client gets "this session has no worktree, so there is nothing to review" — true of the state, but it is a sentence about a different request, which is the rule the repo applies everywhere else (`ApiError::no_worktree(message)` exists *precisely* so each caller supplies its own — `error.rs:376-384`; `bad_body`/`bad_prompt`/`bad_steer`/`bad_merge_body` are four sentences over one code). ADR-0063 D1 chose the import deliberately, so this is a design note rather than a slip — but D1's stated reason ("a client that read this one on `GET …/changes` should not have to learn a second spelling") is about the *code*, which is shared either way. The next-step halves ("resume the session", "restore the project's checkout") are correct for both routes and should not change.
- **Suggested fix shape:** keep `refusal()` `pub(crate)` and give the merge a thin mapper that matches the four `ChangesError` state variants and answers `ApiError::no_worktree(MERGE_…)` with the same sentences minus the review clause ("this session has no worktree, so there is no branch to merge"), delegating the `Git` arm to the existing function.

---

**Finding: P2 — a stale worktree registration makes the merge refuse for a reason that is not true.**
- **Location:** `crates/holosmith/src/worktree.rs:582-584` and `:584-602`; the refusal it produces is `crates/holosmith/src/merge.rs:277-285`.
- **What & Why:** `checkouts` documents that "`locked`/`prunable` are ignored rather than interpreted", and `checkout_of` (`merge.rs:340-346`) matches on the branch line alone. A repository whose target is registered in a **prunable** worktree — the directory deleted, `git worktree prune` not yet run — therefore answers `409 target_checked_out` with "check out another branch in that worktree and merge again" (`http/merge.rs:83-85`), advice that cannot be followed because the directory is gone, and refuses a merge the operator could perform. That is the same failure ADR-0063 D6 argues against in the dirty case ("refusing a merge the operator cannot perform for a reason that is not true"), and this build is full of prunable registrations (the HS-052 landmine is exactly a `git worktree prune`).
- **Suggested fix shape:** treat a block carrying `prunable` as not-a-checkout in `checkouts` (add a `prunable` flag to `Checkout` and skip those in `checkout_of`), with one unit test over a hand-written porcelain listing; the merge then takes the `update-ref` arm, which is correct for a worktree with no working tree.

---

## Spec axis

**Verdict: OK with notes (all six acceptance bullets are met and measured; the ADR's D7 paragraph must be corrected before HS-055 quotes it).**

Bullet-by-bullet, verified: body/`strategy` default (`http/merge.rs:130-160`, `tests/merge.rs:185-215`, `:536-551`); `target` listed-or-422, both facts (`merge.rs:240-245`, `:254-256`; `tests/merge.rs:334-368`; `case_merge` step 2 uses `trunk` for the second fact, `verify.sh:5549-5565`); `ff-only` without a temporary worktree and `409 not_fast_forward` (`merge.rs:257-264`; `tests/merge.rs:245-277`); `update-ref <new> <old>` (`merge.rs:369-390`, argument vector pinned `:495-509`); `git merge --ff-only <new>` in the main checkout with `409 target_dirty` (`merge.rs:408-453`); exactly `{merged_commit, target, strategy}` (`merge.rs:112-122`, `tests/merge.rs:509-535` asserts the object has three keys). No container is consulted (the module imports no backend), which is item 12's governing sentence.

---

**Finding: P1 — ADR-0063 D7's paragraph about the changes view is wrong in both of its clauses.**
- **Location:** `docs/adr/0063-the-merge-writes-refs-so-every-answer-is-measured.md:142-145`.
- **What & Why:** "`GET …/changes` answers `ahead: 0` afterwards because `ahead` is `rev-list --count <base>..agent/<id>` and the *target* moved — the branch did not — so what actually changes is `behind` (to `0`) and, for a branch that was behind, `mergeable`." (a) The measurement it cites refutes the first clause: if the branch did not move, `ahead` keeps its old value; the suite's fixture has `ahead: 1` before **and** after (`tests/merge.rs:721`, `:732`). (b) `behind` cannot be nonzero before a successful `ff-only` — a target with commits the branch lacks is `not_fast_forward` (`merge.rs:257-264`), i.e. bullet 3 of the story — so it cannot change "to `0`", and `mergeable` cannot pass from `false` to `true` either (a conflict requires a target commit the branch lacks). The true statement, which the same sentence half-reaches, is that **no** field of the summary necessarily changes on this path, which is precisely *why* not writing anything to the row is the right call. As written, the paragraph tells HS-055 and HS-058 to expect a field to move that never moves.
- **Suggested fix shape:** "…and `GET …/changes` is unchanged by a merge: `ahead` is `rev-list --count <base>..agent/<id>` and the branch did not move, `behind` was already `0` because a fast-forward requires the target to be an ancestor of the branch, and `mergeable` stays `true`. That is recorded here because "a merge changed nothing in the answer" is the intuitive reading and it is the correct one." Then fix the two module docs at `merge.rs:34-36` / `http/merge.rs:23-25` to match (standards P1 above), and the test name at `tests/merge.rs:704`.

---

**Finding: P2 — the third answer to rule 4's two-arm question is a real decision, but it is decided in code and pinned nowhere.**
- **Location:** `crates/holosmith/src/merge.rs:277-285`; `docs/adr/0063-…md` D5 (`:92-110`).
- **What & Why:** the story's bullets 4 and 5 name exactly two shapes (checked out nowhere; checked out in the main checkout) and two refusals (`409 not_fast_forward`, `409 target_dirty`). The code adds a third state and a third `409` (`target_checked_out`), justified well in D5 — writing a session's worktree from the host moves files under a container — and the honesty of the justification is not in question. What is missing is the *contract* half: ADR-0063's "What this ADR does not decide" (`:196-206`) does not list it, so the next story (HS-058's sheet, which must render this refusal) has no ADR line telling it the state exists, and no test asserts the status/code pair anywhere (standards P2 above). Note too that the state is reachable only when the project's own checkout has left the target (`git` refuses the same branch in two worktrees) — which is exactly the operator action the sentence asks for, so the sentence's advice is right.
- **Suggested fix shape:** add one line to ADR-0063's "does not decide" (or to D5) naming `target_checked_out` as a fact the UI must render and HS-056/HS-058 inherit, and one wire assertion as described above.

---

**Finding: P2 — ADR-0063 D3's heading contradicts its own order, and the code keeps both orders.**
- **Location:** `docs/adr/0063-…md:73-77`; `crates/holosmith/src/http/merge.rs:116-120` and `crates/holosmith/src/merge.rs:240-251`.
- **What & Why:** D3 is titled "Request validity before session state" and then orders `body → row → the row's base_commit (409 no_worktree) → the target is in merge.targets (422) → the four basis refusals` — i.e. a session-state refusal ahead of a request-validity one, which is the opposite of what the heading claims and of ADR-0012's precedent it cites. The code keeps it, so one wrong request gets two answers depending on the *session's* shape: `{"target":"nope"}` on a row with no `base_commit` → `409 no_worktree` (handler, before the target is ever looked at), and the same body on a row whose directory is gone → `422 bad_target` (module's first check runs before `basis`). The order is recorded (D3 and `merge.rs:214-235`), so this is a correctness-of-contract note, not a discovery gap.
- **Suggested fix shape:** validate `target` membership in the handler immediately after `parse_body` — it is pure and needs no repository, and `state.config.merge.targets` is already in hand — so the order is uniform and matches the heading: body → row → target listed (`422`) → `base_commit` (`409 no_worktree`) → basis → branch → ancestry → write, with D3 updated to that one list.

---

**Finding: P2 — the commit the target advances to is decided before two reads that can move it.**
- **Location:** `crates/holosmith/src/merge.rs:251` (`let head = basis(worktree)?`), `:257-264` (ancestry test reads `refs/heads/<branch>` fresh), `:268` and `:273` (write and answer use `head`).
- **What & Why:** `head` is the branch tip as of `basis`; the ancestry test then re-reads the branch **ref** (not `head`), and the write uses `head`. A commit landing on the branch between those calls makes the merge fast-forward the target to a commit that is not the branch tip, answer `200` naming it, and leave the branch's newest commits unmerged — reachable in this build precisely because HS-056's `session_running`/`dirty_worktree` guards are deliberately absent (`merge.rs:233-235`, ADR D3's last paragraph). `GLOSSARY.md:550`'s "the target is moved to the session branch's tip" then is not true, and neither arm has a postcondition on the *branch* side (the postcondition at `merge.rs:418-433` measures the target only).
- **Suggested fix shape:** resolve the branch tip once, inside the lock, and use that same sha for both the ancestry test (`is_ancestor(&reference, &head_sha)`) and the write, so the test and the write are about one commit; the `update-ref` arm then also gets the guarantee its compare-and-swap was designed around.

---

**Merge verdict: OK with notes.** No P0. The three P1s (the symmetric changes-view test and its wrong name, the dead `MergeError::sentence` behind a doc that promises logging, and "`ahead: 0`" in two module docs + ADR D7) are small and self-contained; I would land them before HS-055 builds a temporary worktree on top of this module's refusal and diagnostic machinery. Test commands a supervisor must run: `cargo test -p holosmith --test merge`, `cargo test -p holosmith` (the `merge.rs`/`http::merge` unit tests), and `scripts/verify.sh case_merge` (needs Docker and `python3`).
## Resolution

| # | Finding | Verdict |
| --- | --- | --- |
| St P1-1 | `the_sessions_changes_view_answers_ahead_zero_after_a_merge` cannot fail (every post-merge value equals its pre-merge value) and its name asserts the opposite of its body | **Fixed.** Renamed `a_merge_leaves_the_changes_view_unchanged`; it now asserts the summary is unchanged field for field **and** that `refs/heads/main` is the branch's tip, which a merge that wrote nothing cannot produce. Verified by mutation: making `merge()` return before the write turns it red along with ten others. The comment states the real contract (the branch did not move, `behind` was already `0`). |
| St P1-2 | `MergeError::sentence()` has no caller behind a doc that promises logging, and a failed merge **write** is logged by `http::changes::refusal` as "the changes view could not read the repository" | **Fixed.** `refusal_of` logs `sentence = error.sentence()` at `warn` before answering, so the paths and the configured list reach the log; the `ChangesError::Git` arm is now answered by this route's own `changes_refusal` as `internal("merge", …)`, with this route's operation word. |
| St P1-3 | two module docs (and ADR D7) claim `GET …/changes` answers `ahead: 0` after a merge | **Fixed** in `merge.rs`, `http/merge.rs`, ADR-0063 D7 and the landmines: `ahead` counts the branch's commits and the branch did not move, `behind` was already `0` (a target ahead is `not_fast_forward`), and the correct statement — the summary is **unchanged** — is what the test now asserts. |
| St P2-1 | `409 target_checked_out` is pinned by no status/code assertion, and the module doc claims "six refusals" at the wire | **Fixed.** `a_target_checked_out_in_another_worktree_is_a_409_at_the_wire` asserts `409` + `target_checked_out`, that the target did not move and that the worktree the sentence says it will not touch is untouched; the doc no longer counts. |
| St P2-2 | the shared `no_worktree` sentences end in "there is nothing to review" on a route that was asked to merge | **Fixed by giving the merge its own sentences.** `http/changes.rs`'s `refusal` and `NO_WORKTREE` are private again; `http/merge.rs` writes four messages of its own over the same code ("…so there is no branch to merge"), and ADR-0063 D1 records the correction and why the *code* is the part that is shared. |
| St P2-3 | a **prunable** registration makes the merge refuse for a reason that is not true | **Fixed.** `worktree::Checkout` carries `prunable`, `parse_checkouts` reads it (unit test over all four block shapes), and `checkout_of` filters it out so the merge takes the `update-ref` arm. `a_prunable_registration_is_not_a_checkout_and_the_merge_lands_by_the_ref` removes a checkout's directory and merges anyway; dropping the filter turns exactly it red. |
| Sp P2-1 | ADR D3's heading contradicts its own order, and one bad `target` gets two answers depending on the session's shape | **Fixed.** The handler asks `merge::listed` immediately after the body (pure, and `merge.targets` is in hand), so the order is body → target listed → row → `base_commit` → basis → branch → ancestry → write; `merge()` asks the same predicate again as its own precondition. D3, `merge.rs`'s doc and `http/merge.rs`'s doc all state that one list. |
| Sp P2-2 | the commit the target advances to is decided before two reads that can move it | **Fixed.** The ancestry test is asked about `head` — the commit the merge will write — and `fast_forward`'s re-measurement uses it too, so the test, the write and the diagnostics are about one commit. Recorded in ADR D4 and the landmines, with the reason it is reachable (HS-056's guards are absent by design). |
| Sp P2-3 | the third answer to rule 4's two-arm question is decided in code and pinned nowhere in the contract | **Fixed.** The wire test above pins it, and ADR-0063's "what this ADR does not decide" now names `target_checked_out` as a fact HS-058 must render and HS-056 must not lose. |
| — | the task's own list, checked rather than assumed | **Nothing else found.** `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean; the gate re-run's counts are in the ship note on the issue. |
