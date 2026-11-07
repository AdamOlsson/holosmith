# PER-87 — HS-055, the merge's `merge` and `squash` strategies

Review of `3d638b8...HEAD` (commit `94252e9`), run by a fresh `reviewer` child against
`/tmp/holosmith/reviews/PER-87-diff.txt`, with the spec axis taken from the Linear body, the story slice
(`docs/stories/phase-2-deployable-mvp.md`, HS-055) and spec item 12's rules 2, 3 and 6. Saved verbatim; the
`## Resolution` table below is the only edit.

## Review

Two axes, as the brief asks. Sources verified: `HolosmithSpec.md` §12 (rules 2, 3, 5, 6), `docs/stories/phase-2-deployable-mvp.md:138-150` (HS-055) and `:187-198` (HS-058), `docs/adr/0063`, `docs/adr/0064`, `docs/notes.md:1957-2026`, `RESUME.md:101-241`, `AGENTS.md`, and the shipped code at HEAD.

---

## Standards

**Correct (verified)**
- **One write path kept.** `merge.rs:531-536` still dispatches only to `update_ref`/`fast_forward`; the new `Site` enum (`merge.rs:461-476`) is two arms of the *same* rule-4 commands, and `listed` (`merge.rs:597-603`) remains the single "is this a target" predicate used by both module and handler (`http/merge.rs:135`). No second ref-mover, no second predicate.
- **The envelope's additivity claim is measured, not asserted.** `ErrorBody.conflicts` with `skip_serializing_if` (`http/error.rs:157-166`), plus the assertion that *every* non-conflict body lacks the key at `tests/merge.rs:1169-1175` in `every_bad_request_moves_no_ref` (`tests/merge.rs:1144`). That is ADR-0052's `kept` shape, correctly reused.
- **The teardown-before-the-move decision is structurally enforced**, not comment-enforced: `land` (`merge.rs:537-555`) computes `landed` and calls `remove` (`merge.rs:553`) with no `?` between them, so every failure inside `apply` and the later `409 target_dirty` both leave `<data>/merge` empty. Measured by `the_temporary_worktree_is_gone_even_when_the_move_is_refused_after_it_landed` (`tests/merge.rs:896`).
- **The hook tests can actually fail.** Hooks are written into the *common* `.git/hooks` (`tests/merge.rs:929+`), and `support::TempDir::file` only joins a path (`tests/support/mod.rs:52-54`) — the marker is not pre-created, so a hook firing turns the test red. `landing_prefix` (`merge.rs:181-195`) is pinned verbatim by `the_landing_prefix_disables_hooks_and_carries_this_build_s_identity` (`merge.rs:955`).
- **Error classification stays measured:** non-zero exit **with** unmerged paths = conflict, without = `500` with git's words (`merge.rs:616-636`), and `nothing_staged` reads `diff-index --cached --quiet`'s two codes rather than English (`merge.rs:675-694`).

**Finding: P1 — the squash-vs-keep-rule interaction this story was told to decide is neither recorded nor tested.**
- Evidence of the obligation: `RESUME.md:1071` — *"both merge strategies must leave `agent/<id>` merged — a squash that keeps the branch's commits out of the target is still 'merged' only by tree, which is why `merged_into` uses `merge-base --is-ancestor` and a squash fails it. **Decide what that means for the keep-rule and record it.**"*
- The predicate is exactly as described: `worktree.rs:312` (`merge-base --is-ancestor agent/<id> refs/heads/<target>`), and `retained` keeps the branch on `Merged::No` (`worktree.rs:199-202`). After a `squash`, the target's new commit's only parent is the old target, so the branch tip is *not* an ancestor ⇒ `Merged::No` ⇒ branch kept.
- The existing keep-rule test pins the opposite assumption via a fast-forward: `tests/worktree.rs:452-464` ("a fast-forward, which is what `git merge` does while the target has not moved") asserts `branch: None`.
- `docs/adr/0064` has no section on this, and its "What this ADR does not decide" sends rule 5 to HS-056 "still `422 unsupported` with no new sentence" (`docs/adr/0064…md:216-223`); `docs/notes.md`'s HS-055 landmines (`notes.md:1957-2026`) are silent. `HolosmithSpec.md:230` (rule 5: "The branch `agent/<id>` is then deleted because it is merged") therefore becomes unsatisfiable for one of the three strategies this story ships, and HS-056's acceptance bullet is built on it (`phase-2-deployable-mvp.md:164`).
- Smallest fix: one ADR-0064 D-section (and one notes landmine) stating that a `squash` leaves the branch a non-ancestor, so `retained` keeps it — with HS-056 named as the owner of the resolution — plus one assertion in `tests/merge.rs` that `worktree.retained(...)` after a squash answers `Known { branch: Some(..) }`. No production code change required.

**Finding: P2 — a code-doc sentence the change made false (the third of its class).** `http/merge.rs:29-31` still says `GET …/changes` is unchanged *"and `behind` was already zero because a fast-forward requires the target to be an ancestor of the branch."* For the two new strategies the target gains a commit the branch lacks, so `behind` = 1 (`changes.rs:768-770`, `rev-list --count branch..target`) and `mergeable` stays `true`. `merge.rs:46-48` was correctly narrowed to `ahead` only; `http/merge.rs` was not.

**Finding: P2 — three more stale code docs from the same class.**
- `http/error.rs:123-126`: `UNSUPPORTED` — "`merge` and `squash` (HS-055) and `delete_session` (HS-056) are in the contract and not in this build". Two of the three are built.
- `http/error.rs:429-433`: `unsupported()`'s doc — "`strategy: \"squash\"` and `delete_session: true` are two requests whose next steps differ". There is now one.
- `http/merge.rs:1` ("spec item 12's **rules 2 and 4**") and `http/merge.rs:12` ("the **two** `unsupported` sentences"); and `tests/merge.rs:1` ("The merge's **fast-forward path** … (HS-054, spec item 12's rules 2 and 4)") — rule 3 and both strategies are this story's.

**Finding: P2 — the gate's own comment now miscounts its refusal table.** `scripts/verify.sh:5548-5550`: *"so the two `422`s that remain are the parameter's (`bad_target` twice, for the two different facts) and the one field this build still cannot honour"* — but the loop it introduces (`verify.sh:5552-5557`) also walks `strategy:"rebase"` → `bad_strategy`, so it enumerates three facts and counts two.

**Finding: P2 — the `prune` arm is claimed but unmeasured.** `merge.rs:708-726` exists for the shape "a registration whose directory is gone"; `docs/notes.md:1975-1981` says as much and admits *"keeping it but skipping `prune` turns it green and leaves the rare half unhandled."* `a_stale_temporary_worktree_from_a_killed_run_is_removed_and_the_merge_lands` (`tests/merge.rs:868`) only covers the *unregistered directory* arm (`clear`'s `remove_dir_all`). Smallest fix: in that test, register a worktree (`git worktree add`) and then `rm -rf` its directory without pruning — that arm goes red if `prune` is dropped.

**Finding: P2 (judgement) — a duplicated 12-line parser and a duplicated sentence.** `merge::conflict_paths` (`merge.rs:654-673`) vs `changes::conflict_paths` (`changes.rs:837-848`); `MergeError::Conflict`'s sentence (`merge.rs:409-415`) vs `changes::conflict_sentence` (`changes.rs:851-858`). Report-only: the two parsers read different wire formats (line-oriented `merge-tree` stdout that *stops* at the first tab-less line, vs NUL-delimited `-z` records that are *skipped*), so sharing would need a format flag and would widen a private helper. ADR-0064 D5 already says "the same shape … one command over".

---

## Spec

**Correct (verified against the five acceptance bullets)**
- Bullet 1 (temp worktree at `<data>/merge/<id>` from the target, removed afterwards even on failure): `merge.rs:537-555`, measured three ways — `temporary()` empty after success, after a conflict (`tests/merge.rs:823`), and after a *post-landing* refusal (`tests/merge.rs:896`), plus the gate's `find … -mindepth 1 | wc -l` (`verify.sh:5855-5861`, with the "the directory exists first" guard that stops a never-created dir passing).
- Bullet 2 (`-c core.hooksPath=/dev/null`): on the fast-forward, the landing and the commit — `merge.rs:173-195` — and hooks live in the common git dir, so the flag is load-bearing, not decorative.
- Bullet 3 (one merge per project; waits or 409): waits, reused from HS-054, with a client-visible measurement `two_merges_of_one_project_serialise_and_both_land` (`tests/merge.rs:1016`) that goes red without the lock (both landings would be built from the same `old` and the second move would fail).
- Bullet 4 (conflict → 409 `conflict` with the paths, nothing changes): `merge.rs:343-369`, `http/error.rs:156-166`, and both module (`tests/merge.rs:823`) and wire (`tests/merge.rs:1341`) tests measure the target ref, the checkout's `status --porcelain` and the operator's file.
- Bullet 5 (Resolve with agent) — dismissal judged **honest and complete**: HS-058's bullet 6 is literally "Conflict state shows the paths and the 'Resolve with agent' action" (`phase-2-deployable-mvp.md:198`), so the affordance has a named owner, and what HS-055 builds (`409 conflict` + `conflicts`) is exactly its input. ADR-0064 D10 (`docs/adr/0064…md:186-195`) states the pointer rather than ticking.
- Rule 6's `{merged_commit, target, strategy}` with the strategy that **ran**: `merge.rs:388-401`, asserted at the wire including `body.as_object().len() == 3` (`tests/merge.rs:1275`).
- The gate proves rather than narrates: a squash that silently fast-forwarded fails `rev-list --count` `1`; a `merge` that fast-forwarded fails `rev-list --parents` `"<new> <squashed> <tip>"`; a swallowed conflict fails `expect_status 409`/`expect_code conflict`.

**Finding: P2 — an undocumented, untested wire-code change.** `{"strategy": 3}` (any non-string, non-null value) now answers `422 bad_body` (`http/merge.rs:190-196`, pinned at `http/merge.rs:351-361`); HS-054 answered `422 bad_strategy` for the same body. ADR-0008 makes `code` effectively an API, and neither ADR-0064 nor `notes.md` records the reclassification, nor does the wire test table (`tests/merge.rs:1144-1175`) cover it. Fix: one line in ADR-0064 (D3 or D9), or keep `bad_strategy` for a non-string; either way assert it at the wire.

**Finding: P2 — the `ff-only` refusal order changed silently.** The restructure moved `checkout_of` above the ancestry test (`merge.rs:474` vs `merge.rs:492`), so for `ff-only` a branch that is *behind* and whose target is checked out in a session's worktree now answers `409 target_checked_out` where HS-054 answered `409 not_fast_forward` — and the new sentence's next step ("check out another branch in that worktree and merge again") leads straight to the second refusal. No test pins the combination (`a_target_checked_out_in_another_worktree_is_refused`, `tests/merge.rs:466`, uses a fast-forwardable branch, so it passes either way) and ADR-0064 is silent. Fix: ask the ancestry test first for the `None` landing, or record the reordering in ADR-0064 as deliberate.

**Finding: P2 — ADR index count.** `docs/stories/README.md:117` says "Sixty-one decisions were made…" while the list it introduces holds **64** bullets (`:119-182`), including the ADR-0064 line this diff added. It was already stale by two; this diff continues it. (`docs/build.md:245` and the `case_merge` paragraph were updated correctly, so the omission is isolated.)

---

## Dismissed (and why)
- **A second "way to move a ref" / second target predicate:** none — see Standards/Correct.
- **`add`-fails leaving a directory:** bounded, not a defect. `git worktree add` cleans up its own failure, and anything it leaves is exactly what `clear` removes on the next merge (`merge.rs:708-726`); ADR-0064 D1 records "removed, never refused".
- **`prune` harming live worktrees:** `prune` only touches records whose directory is absent, and HS-052 already treats a prunable registration as "not a checkout" (`merge.rs:787-801`). The risk I could not verify is git's default `gc.worktreePruneExpire` on a freshly-orphaned record; that is a reason to *add* the missing test (P2 above), not to change the code.
- **D3's measurement (git's default message):** the exact string can no longer be re-measured because `-m` is always passed, but the *decision* is enforced — `a_merge_is_no_ff_and_always_carries_a_message` (`merge.rs:1006`) pins the vector, so dropping `-m` goes red. The identity claims are measured by reading `%an <%ae>` off both strategies' commits (`tests/merge.rs:623`, `:672`), which is the only way they can be, since this machine's config is `Adam Olsson`.
- **D10's quote:** the substance is right (ADR-0046:71-76 cut the checkbox's copy because "it is a new visible element, and it belongs to the story that owns the form"), but the phrase in quotation marks — "a new element is a UI change" — appears verbatim in `notes.md:367`/`RESUME.md:883`, not in ADR-0046. Cosmetic; worth a bare `(cf.` if the ADR is touched for the P1 anyway.
- **Gate SKIP:** `case_merge_strategies` skips only on missing `python3` for the Docker preflight (`verify.sh:5680-5684`) — the same guard `case_merge` uses, and not a skip on the story's own subject.

**One-line summary.** Standards: 1 P1 (missing recorded decision for squash vs the keep-rule) and 5 P2s, worst = the unrecorded cross-story hazard. Spec: 0 P1s, 3 P2s, worst = the fs-only reordering of `ff-only`'s refusals; the deferred fifth acceptance bullet is honestly and completely handed to HS-058.

**Merge verdict: BLOCK** — on the one P1 only. The fix is additive (an ADR section, a notes landmine, one assertion); nothing in the shipped behaviour is wrong, and no test command needs re-running beyond `cargo test -p holosmith --test merge --test worktree` for the added assertion.
---

## Resolution

Every finding is answered. Nothing is dismissed without a citation; two P2s are judged report-only and say why.
The verdict was BLOCK on the one P1, which is fixed (an ADR section, a notes landmine and an assertion — no
production code change, as the review predicted).

| # | Axis | Finding | Verdict | Where |
| --- | --- | --- | --- | --- |
| 1 | Standards | **P1** — the squash-vs-keep-rule interaction `RESUME.md` demanded be decided is neither recorded nor tested, making rule 5's "the branch is deleted because it is merged" unsatisfiable for one strategy | **Fixed** — ADR-0064 **D11** records that a squash leaves the branch a non-ancestor, that this is the honest answer (the branch holds the session's commits, and a squash deliberately leaves them out), and that HS-056 decides rule 5 from the merge it just performed rather than by re-asking git. A notes landmine says the same. `a_squash_keeps_the_branch_a_merge_does_not` asks `Worktree::retained` after each strategy: `branch: None` after a `merge`, `branch: Some(agent/<id>)` after a `squash` | `docs/adr/0064-…md` D11, `docs/notes.md`, `crates/holosmith/tests/merge.rs` |
| 2 | Standards | P2 — `http/merge.rs:29-31` still claimed `behind` was zero because a fast-forward requires ancestry, which is false for the two new strategies | **Fixed** — the paragraph now says `ahead`/`dirty` are unchanged and `behind`/`mergeable` were never this route's claim to keep: after a landing the target has a commit the branch lacks and `behind` is the `1` that says so | `crates/holosmith/src/http/merge.rs` |
| 3 | Standards | P2 — three stale code docs (`UNSUPPORTED`'s const doc, `unsupported()`'s doc, `http/merge.rs`'s title and "the two `unsupported` sentences", `tests/merge.rs`'s title) still describe two unsupported fields and a fast-forward-only route | **Fixed** — all five re-stated for the world after this story; the const's doc names `delete_session` as the one field left and says the strategy arm went with HS-055 | `crates/holosmith/src/http/error.rs`, `crates/holosmith/src/http/merge.rs`, `crates/holosmith/tests/merge.rs` |
| 4 | Standards | P2 — `scripts/verify.sh:5548-5550`'s comment said "the two `422`s that remain" over a loop that walks three facts | **Fixed** — the comment now counts three facts on two codes (`bad_target` twice, `bad_strategy`, `unsupported`) | `scripts/verify.sh` |
| 5 | Standards | P2 — the `prune` arm of `clear` was claimed in ADR-0064 D1 and unmeasured | **Fixed, and it found a defect** — `a_stale_temporary_worktree_from_a_killed_run_is_removed_and_the_merge_lands` gained the *registered-but-gone* half (`worktree add`, then `rm -rf` of the directory). Dropping `prune` from `clear` turns it red (measured). Two of its assertions were **vacuous** on the first attempt — `listing.contains("prunable")` was satisfied by the fixture's own label `strategy-prunable` inside the temp path — so the needle is now `"prunable "` with git's reason, the label is `strategy-registered`, and HS-054's identically-defective `a_prunable_registration_is_not_a_checkout_and_the_merge_lands` (fixture `merge-prunable`) is fixed in the same commit | `crates/holosmith/src/merge.rs`, `crates/holosmith/tests/merge.rs`, `docs/adr/0064-…md` D11, `docs/notes.md` |
| 6 | Standards | P2 (judgement) — `merge::conflict_paths`/its sentence duplicate `changes::conflict_paths`/`conflict_sentence` | **Dismissed, with the reason the review gives** — the two parse *different wire formats* (line-oriented `merge-tree` stdout that stops at the first tab-less line, vs NUL-delimited `ls-files -z` records that are skipped), so sharing would need a format flag on a private helper and would widen a private surface. ADR-0064 D5 already records the relationship ("the same shape one command over"), and `changes::conflict_paths` is private to a module that promises "no write" | `crates/holosmith/src/merge.rs`, `docs/adr/0064-…md` D5 |
| 7 | Spec | P2 — `{"strategy": 3}` reclassified from `bad_strategy` (HS-054) to `bad_body`, undocumented and untested at the wire | **Fixed** — the reclassification is deliberate (a non-string is the *body's* shape, the create's own split for a numeric `model`), ADR-0064 D11 records it, and `every_bad_request_moves_no_ref`'s table asserts it with the other six | `crates/holosmith/tests/merge.rs`, `docs/adr/0064-…md` D11, `docs/notes.md` |
| 8 | Spec | P2 — the restructure moved `checkout_of` above the ancestry test, so `ff-only` now answers `target_checked_out` where HS-054 answered `not_fast_forward`, and no test pinned the combination | **Fixed in code, not just recorded** — the ancestry test moved back above the checkout question *for the default strategy*, so HS-054's order is preserved exactly ("check out another branch in that worktree" would otherwise be advice leading to a second refusal); the landings keep the checkout question first so a `409` costs no temporary worktree. `the_fast_forward_refusal_comes_before_the_checkout_one_and_the_landings_do_not` is one repository answering both ways, and ADR-0064 D6 states the split | `crates/holosmith/src/merge.rs`, `crates/holosmith/tests/merge.rs`, `docs/adr/0064-…md` D6 |
| 9 | Spec | P2 — `docs/stories/README.md:117` said "Sixty-one decisions" over 64 bullets | **Fixed** — "Sixty-four", which is also the number of files in `docs/adr/` | `docs/stories/README.md` |
| 10 | Standards | P2 (cosmetic) — ADR-0064 D10 quoted "a new element is a UI change" as ADR-0046's, but that phrasing is the notes'/RESUME's | **Fixed** — the sentence now cites ADR-0046's actual argument (the copy was a new visible element belonging to the story that owned the form) and points at the notes for the rule's wording | `docs/adr/0064-…md` D10 |

Mutation pass over the new claims, all measured on this tree: dropping `--no-ff` turns three tests red; removing
`hooks_flag`'s `-c core.hooksPath=/dev/null` turns two red; moving the teardown after the move turns eight red;
skipping `clear`'s removal turns one red; dropping `prune` turns one red; removing the identity pairs turns two
red; answering "no unmerged paths" turns two red; running the ancestry test for every strategy turns five red.
