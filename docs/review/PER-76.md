# Review of PER-76 (HS-044, startup reconcile and crash safety, V9)

Fixed point `a08872f`; reviewed range `a08872f..07b5bb8` (the two commits this story made before the
review), plus the reviewer's reading of the tree. The reviewer had the diff at
`/tmp/holosmith/reviews/PER-76-diff.txt`, the Linear body and story slice as the spec axis, and
`AGENTS.md`, `docs/build.md`, `docs/notes.md`, `docs/stories/README.md`, the ADRs and the crate's own doc
comments as the standards axis.

The review is saved verbatim below; `## Resolution` follows it.

---

## Review — Standards (how this repo writes code)

**Correct (verified, not assumed)**
- The one-writer rule holds: the crash transition goes through `Transitioner::apply` (`reconcile.rs:329-343`) and the settle through `Provisioner::with_settle_reason` → `settle` → the same function (`provisioner.rs:277-283`). No `set_status` in `src/`.
- ADR-0035 honoured: `docker ps -a --filter name=s_ --format '{{.Names}}'` / `network ls … '{{.Name}}'` (`docker.rs:286-338`), one name per line, nothing parsed; the substring filter is not the rule — `sessions::is_id` is (`reconcile.rs:130-141`). ADR-0052 honoured: `Retained::Unknown` keeps the branch (`worktree.rs:227-240`).
- Docs the story owes are present: `docs/adr/0053`, the `docs/notes.md` landmine section, `docs/stories/README.md` (52→53 + the ADR line), the V9 row.

**Finding P1 — `docs/build.md` is not updated.** Its bolded rule ("Every later story adds at least one `case_*` function and one line in `serve_cases` … a case that needs a setting the rest of the run must not carry gets a boot of its own (see `case_token_budget`, `case_usage_survives_resume`, …)"; `build.md:296-306`) and notes.md's "`docs/build.md` is a story deliverable … If a story changes prerequisites or gates, the doc is in scope" are both broken: neither `case_reconcile` nor `case_reconcile_orphans` appears anywhere in `build.md`, though both boot their own server and one needs the daemon *and* the image (the `case_delete_keeps_the_work` sentence at `build.md:252-259` is the shape). Smallest fix: one clause in the `serve_cases` enumeration and one in the "Optional: a Docker daemon" prerequisite line.

**Finding P2 — three comments say the pass is a task.** `reconcile.rs:46-51` ("The reconcile is a task rather than a step before the banner … never the boot") contradicts the same file's module doc ("awaited **before the listener answers anything**", `reconcile.rs:20-26`), `serve.rs:303-313` and ADR-0053; `scripts/verify.sh:1901` repeats it ("The sweep is a
task, so it is waited for"). The "never the boot" claim is also false in aggregate: the pass is serial and awaited, so a wedged daemon costs 10 s per listing *and* per leftover name. Fix the three sentences.

**Finding P2 — Duplicated Code (smell).** `Docker::remove_network_bounded` (`docker.rs:275-278`) is `remove_container_bounded` (`docker.rs:229-232`) with different arguments; `tests/support/mod.rs:593` re-implements the `worktree ` parse that `worktree::registrations` now owns — the module whose doc says one spelling.

**Finding P2 — Speculative Generality + a test that cannot fail.** `reconcile::plan`/`Plan` (`reconcile.rs:96-102`) have no production caller: `run` and `sweep` call `recoveries`/`containers_to_remove`/`networks_to_remove` directly. Its only test, `a_second_pass_over_what_the_first_removed_removes_nothing` (`reconcile.rs:594-606`), feeds `plan(&rows, &[], &[])`, so "removes nothing" cannot fail, and it tests code the server never runs — against notes.md's "Mutating the rule and watching the test fail is the only proof". (`a_worktree_a_row_owns_is_not_an_orphan`'s last line, `assert!(Retained::Unknown.keeps_branch())`, is the same shape.) The other seven tests do bite: delete `with_settle_reason` and `…says_the_turn_was_interrupted` loses its payload; add `Status::Stopped` to `recoveries` and `the_rows_a_crash_cannot_own…` goes red.

**Finding P2 — `GLOSSARY.md` has no `startup reconcile` entry**, though `Sweeper`, `Compaction` and `Provisioner` all do; notes.md calls it "the vocabulary authority … check it before inventing a term".

**Merge verdict: OK with notes** (P1 doc gap first).

---

## Review — Spec (what was asked)

**Correct.** All five acceptance bullets have code: `ps -a --filter name=s_` → containers removed → live rows restarted (`reconcile.rs:369-410`, restarts last, `:351-366`); `starting` rows resumed through the provisioner and failed to `error` with the step's sentence; `agents_*` and `.worktrees/<id>` removed only when no row owns the id; `interrupted` written as a reason on `session.status` (and refused as a state by `Status::ALL`); `last_seq` untouched. Non-goals respected — no `docker events` watcher, no `doctor`, no drain, no create/resume/delete change beyond the additive `with_settle_reason`/`last_seq`.

**Finding P1 — "restart sessions that were live" is not guaranteed; the ADR says it is.** `serve.rs:275` spawns the idle-stop sweeper, whose first pass is immediate (`sweeper.rs:110-120`, `candidates()` = `idle_before(now - idle_stop_min)`), *before* `serve.rs:313` awaits the reconcile. On a boot whose live `idle` row is older than `idle_stop_min`, the sweeper can take it first: the reconcile's `CrashRecovery` then returns `Illegal`, logs `warn "a live row could not be moved back to starting"`, and the restart's `still_starting` returns early (`provisioner.rs:257-276`) — so the row ends `stopped`, not restarted. ADR-0053 and `reconcile.rs:326-328` assert the opposite ("A live row is `starting` before the sweeper's first tick can stop it"). Smallest fix: await the reconcile before `state.sweeper()` is spawned. (No data is lost; the end state converges to `stopped` — but the ordering guarantee and the acceptance line do not.)

**Finding P1 — the V9 gate can go green with V9 unproven.** `case_reconcile_orphans` (`verify.sh:1826-1839`) SKIPs without a daemon *or* the image and proves exactly what `tests/reconcile.rs` deliberately cannot: leftover container gone, orphan `agents_*` gone, the live session back under `s_<id>` with a new id. On a hub — a documented supported machine — acceptance bullets 1 and 3 (containers/networks) are then proven nowhere and `verify.sh` still exits 0. `case_reconcile`'s `python3` skip is defensible: `a_recovery_never_resets_the_seq_counter` covers the counter and `PRIMARY KEY (session_id, seq)` makes duplicates unstorable.

**Finding P2.** `case_reconcile`'s `kill -9` races the stub's settle (four 200 ms frames ≈ 800 ms) and never re-asserts `running` at the moment of the kill; under load `_back/reason` fails for a reason that is not the product's. Kill on the row's own oldest-possible signal, or shorten the race by asserting `running` immediately before `kill`.

**Merge verdict: OK with notes.**

---

## Resolution

| # | Finding | Verdict | What changed |
| --- | --- | --- | --- |
| S1 | `docs/build.md` not updated (P1) | **fixed** | A clause for both cases in the `serve_cases` enumeration (`docs/build.md`, after `case_delete_keeps_the_work`), including what `case_reconcile_orphans`'s SKIP costs and which half no daemon can measure, and `case_reconcile_orphans` added to the "Optional: a Docker daemon and one image" prerequisite. |
| S2 | Three comments still call the pass a task (P2) | **fixed** | `reconcile.rs`'s `BOUND` doc now says the pass *is* the startup path and a wedged daemon costs a bound, a slow boot, never a refused one; the module doc says "before the listener answers anything **and before either policy starts**"; `serve.rs`'s comment says the same with both reasons; `scripts/verify.sh`'s comment says "runs after the banner and before the first request is answered". |
| S3 | Duplicated Code: the two bounded removals; `support::worktrees` reparses `worktree list` (P2) | **fixed** | `Docker::removed_bounded(arguments, bound)` is the one primitive and both `remove_*_bounded` are one line over it; `tests/support/mod.rs`'s `worktrees()` now calls `worktree::registrations`, so the `worktree ` line is parsed in one place. |
| S4 | `plan`/`Plan` has no caller, its test cannot fail, and the `keeps_branch` assert is tautological (P2) | **fixed** | `Plan` and `plan` are deleted (no production caller was the point); the idempotence test is now `a_second_pass_over_the_names_the_first_removed_has_nothing_left_to_take`, which asserts the first pass *does* take each name and the second sees nothing — it goes red if either rule removes nothing or removes everything; the `Retained::Unknown::keeps_branch` line is gone from `tests/reconcile.rs` (that rule is `tests/worktree.rs`'s and `worktree.rs`'s own unit test's). Mutation-checked by hand, and the three checks are these: dropping `with_settle_reason` from the restart fails `a_row_that_was_running_comes_back_idle_and_says_the_turn_was_interrupted` on the reason; adding `Status::Stopped` to `recoveries` fails `the_live_rows_are_the_ones_that_come_back_and_running_is_the_interrupted_one`; and taking the `deleting` exception out of `containers_to_remove` fails `a_container_goes_unless_its_row_is_a_delete_in_progress`. |
| S5 | `GLOSSARY.md` has no entry (P2) | **fixed** | **Startup reconcile** added to *Operations*, next to **Sweeper** and **Delete flow**, with its machine names (`Reconciler`, `Reconciler::run`, `INTERRUPTED`, `Trigger::CrashRecovery`) and its avoid-list. |
| P1 | The sweeper can stop a live row before the crash transition (P1, spec) | **fixed** | `serve` now awaits `state.reconciler().run()` immediately after the state exists and *then* spawns the sweeper and the compactor, so every live row is `starting` before the sweeper has a candidate. The review is right that no data was lost, but "restart sessions that were live" was not guaranteed and ADR-0053 claimed it was; the ADR gained the second reason and `docs/notes.md` a bullet. |
| P2 | `case_reconcile_orphans` SKIPs on a machine with no daemon (P1, spec) | **dismissed, with citation** | The rule the finding asks to prove is a rule about names the daemon holds; a machine without a daemon has none, so the case cannot measure it — the identical shape every Docker case in this run already has (`case_container_mounts` "is the shipped gate's share of V1 only because it is cheap here", `case_docker_resume`, `case_internet_toggle`), and a hub is a supported machine. What *can* be proven without a daemon is proven: `tests/reconcile.rs`'s `a_container_goes_unless_its_row_is_a_delete_in_progress` and `a_network_goes_only_when_no_row_has_its_id` (the containment rule), and the worktree half against a real repository. The case's comment now says what the SKIP costs and what remains proven, which is the part that was actually missing. |
| P3 | `case_reconcile`'s kill races the settle (P2, spec) | **fixed** | The case already asserted the row was `running` immediately before the kill; it now also reads the dead process's database *after* the kill and fails with a sentence that names the race ("the turn had settled before the kill landed … the machine was too slow") instead of blaming the product. The window is the stub's 800 ms turn against one HTTP round trip. |

### Not found by the review, found by the gate (recorded here so the next reader knows)

- **`case_compaction` failed after this change** ("no message.snapshot row appeared"): a crash recovery parks
  every live row in `starting` for the length of its re-provision, and HS-060's `foldable` excluded `starting`, so
  a restarted machine with a day-old log folded nothing until the next 60-second tick. `foldable` now includes
  `starting` — ADR-0032's own argument (no turn is running, no removal is in flight, the pass takes the same
  mutex) — with its unit test updated, a paragraph in ADR-0053 and a bullet in `docs/notes.md`.

### Verdict

**OK with notes → shipped** after the fixes above. The two P1s were both real: the doc gap and the sweeper
ordering (the second was the one the ADR had claimed and not enforced). One P1 was dismissed with the run's own
precedent and a comment that now states the limitation.
