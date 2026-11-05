## Review

Read-only review of the PER-74 / HS-042 WIP snapshot diff (`134d0e8..fcd37dc` artifact: `/tmp/holosmith/reviews/PER-74-diff.txt`). Nothing was modified. Per your steering I stopped doc archaeology and report what the code, tests and gate case give me.

I verified the substance of the core change against `HolosmithSpec.md:160-180` (item 9) and the HS-042 acceptance (`docs/stories/phase-1b-docker-backend.md:304-318`).

---

## Standards

**Correct**
- `snapshot.rs` follows the module conventions it claims: `std`-only, synchronous, one answer per call, module doc that states the rule the code makes true (`snapshot.rs:1-25`), and WIP-level "why" comments rather than narration. It also matches `worktree.rs`'s precedent of *not* being async and of being wrapped in `spawn_blocking` by the backend (`docker.rs:1030`, mirroring `docker.rs:283-290`).
- `tracing` targets and field shapes match every existing site: `holosmith::backend` (`docker.rs:1036`), `holosmith::stop` (`stop.rs:57-63`), `holosmith::turn` (`turn.rs:348-356`).
- CLI error style matches `commands/key.rs`/`config.rs` exactly (`holosmith: <group> <action>: <detail>`, `commands/container.rs:16,23,34`) and `commands/mod.rs` keeps its alphabetical list.
- Every test that asserts a *complete* call list was updated (`abort.rs:269`, `delete.rs:378-381,420,472-475`, `eviction.rs:206`, `idle_stop.rs:189`); tests that assert with `contains` (`turn_timeout.rs`, `exit.rs`, `token_budget.rs`) are unaffected, and I checked `abort.rs:201-211` (tail-3 after `wait_idle`) stays correct because the rung-1 ending is `TurnEnd::Aborted`, which does not snapshot.

**Finding: P2 — the git plumbing is duplicated from `worktree.rs`.** `snapshot.rs` re-implements `GitError` (`worktree.rs:400-414`) as `SnapshotError::Git { command, detail }` (`snapshot.rs:64-92`), and re-implements `git_run`/`git_output`/`git_error`/`git_command` (`worktree.rs:340-372`) as `run()` (`snapshot.rs:157-180`) — including the byte-identical sentences `"git is not runnable ({error}): install git 2.38 or newer"` and `"git exited {} without saying why"`. Smallest fix: make `worktree::{git_run, git_command, git_error, git_output}` `pub(crate)` (or move them to a `git` module) and let `SnapshotError::Git` carry `worktree::GitError`.

**Finding: P2 — `tests/snapshot.rs` re-implements the test harness.** `tests/snapshot.rs:9-45` defines its own `TempDir`, `git()`, `repository()` and `worktree()`, all of which exist in `tests/support/mod.rs` (`TempDir` 33-58, `repository` 514-527, `git` 532-552, `head` 555-557) and are used that way by `tests/worktree.rs:26-27`. Worse, `tests/snapshot.rs:46-48` writes `git config user.name/user.email` into the scratch repo, which `support/mod.rs:511-514` explicitly documents as the thing the harness avoids ("The identity is passed on the commit's own command line rather than written into the repository's config"). Smallest fix: `mod support;` and `use support::{TempDir, git, head, repository};` plus a `worktree_add` helper there.

**Finding: P2 — `IDENTITY` is dead and the same literals are hard-coded twice.** `snapshot.rs:38-42` declares `pub const IDENTITY`, and no site reads it (`grep IDENTITY` over `crates/` returns only the definition). `take_inner` hard-codes `"user.name=holosmith"` / `"user.email=holosmith@localhost"` at `snapshot.rs:139-148`, and builds the ref with `format!("{REF_PREFIX}{id}")` (`snapshot.rs:126`) while `reference()` (`snapshot.rs:184-185`) exists for exactly that. Smallest fix: build `commit_args` from `IDENTITY` and call `reference(id)`.

**Finding: P2 — `commit_args.clear()` is a no-op.** `snapshot.rs:152`. The borrow of `tree` ends at the `run(..., &commit_args, ...)` call, so the `mut` + `clear()` pair only exists to keep `mut` needed. Delete both.

**Finding: P2 — a filesystem failure is reported as a git command.** `snapshot.rs:111-113` maps `create_dir_all`'s error through `SnapshotError::git(worktree, &["index"], …)`, so the operator reads `git -C /srv/projects/.worktrees/<id> index: Permission denied`. Smallest fix: a `SnapshotError::Index { path, detail }` variant (or `command: format!("create {}", parent.display())`).

**Finding: P2 — stale comment now contradicted by the diff.** `tests/eviction.rs:142-144` says "a `Stop` is the cap making room, a `Delete` or a `Snapshot` would be data loss", while the assertion the diff wrote four lines below its helper is `[Call::Snapshot, Call::Stop]` (`tests/eviction.rs:206`). Update the sentence to "a `Delete` would be data loss; the `Snapshot` is the stop never losing work (HS-042)".

**Finding: P2 — `docs/build.md` misclassifies the new case.** `docs/build.md:268-270` now lists `case_snapshot` among the cases that each "get a boot of its own" because they "need a setting the rest of the run must not carry". `case_snapshot` boots nothing (no `start_serve` in `scripts/verify.sh:1583-1631`) and carries no setting. Its four assertions also get no prose description, unlike every neighbouring case. Smallest fix: move the name out of that parenthetical and add one described clause in the style of `case_key_proxy`/`case_container_mounts`.

**Finding: P2 — `stop.rs`'s own documented invariant is not reconciled.** `stop.rs:15-19` states the flag "comes **before** the call" and is the reason the routine exists; the diff inserts a git run before the flag (`stop.rs:54-64`), and both the new comment and `ADR-0049:38-41` justify it with "after the stop there may be no process left to write the worktree" — which is not true of this implementation: the snapshot runs on the **host** (`docker.rs:1021-1030`), and the host worktree survives `docker stop` indefinitely. The ordering is still right (spec item 9 says "before any stop"), but the stated reason is wrong and the module doc above it now reads as unmet. Smallest fix: one sentence in `stop.rs:15-19` saying the snapshot precedes the flag because the spec orders it that way, and the container may be gone by the time the stop returns.

**Finding: P2 — `HS_SESSION_ID` is joined into a path unchecked.** `commands/container.rs:27` does `temp_dir().join(format!("holosmith-wip-{id}.index"))` with an env value from inside the container. The repo documents the opposite rule for exactly this shape (`tool.rs:764-766`, "a session that is not a path segment is refused rather than joined"; id format is 16 lowercase hex per `worktree.rs:75-77`). No privilege gain (the container user can already write those files), so report-only; smallest fix: reject an id that is not 16 hex characters before using it.

**Finding: P2 — one temp index per session, so two snapshots for one session can collide.** `docker.rs:1023-1028` always derives `<data>/sessions/<id>/wip.index`, and `snapshot::take` unconditionally deletes that file on entry and exit (`snapshot.rs:114-116`). A turn-settle snapshot and a `Stopper` snapshot of the same session (sweep tick or delete racing the settle) can delete each other's index mid-run, so the loser fails with git's index error, is logged, and leaves no ref. Consequence is a logged error rather than data loss (the delete's own `409 unsaved_work` still guards), but it is silent by design. Smallest fix: a unique suffix per call (`.wip-<pid>-<counter>.index`) or a per-session `tokio` mutex, the ADR-0037 precedent.

---

## Spec

**Correct**
- Acceptance 1: the script is implemented command-for-command over a temporary index — `read-tree HEAD`, `add -A`, `write-tree`, `commit-tree -p HEAD -m 'holosmith snapshot'` with the identity via `-c`, `update-ref refs/holosmith/wip/<id>` (`snapshot.rs:120-154`), with `GIT_INDEX_FILE` on exactly the three commands that touch the index.
- Acceptance 2: tree-equals-HEAD removes the ref instead of writing (`snapshot.rs:127-137`), and unlike the spec's `update-ref -d $ref || true` it first checks with `rev-parse --verify --quiet`, so a missing ref is not an error. Tested (`tests/snapshot.rs:106-133`) and gated (`verify.sh` second half).
- Acceptance 3: branch, working tree and index untouched — asserted in both places (`tests/snapshot.rs:93-101`, `verify.sh` `case_snapshot` checks `rev-parse HEAD`, `diff --cached`, `status --porcelain`).
- Acceptance 4: all three call sites exist — `Turn::finished`'s `Settled` arm (`turn.rs:318-320`), `Stopper::stop` before the flag and the stop (`stop.rs:54-64`), delete unchanged (`http/sessions.rs:974-977`). All stops route through `Stopper` (`ladder.rs:257`, `sweeper.rs:169`, `http/sessions.rs:498`), so "before any stop" is complete.
- Acceptance 5: a delete failure is still the `409 unsaved_work` refusal (`http/sessions.rs:974-977`), and the two new sites report without vetoing.

**Finding: P1 — "runs at every `agent_settled`" is asserted nowhere.** Delete (`delete.rs`), idle stop (`idle_stop.rs:189`), eviction (`eviction.rs:206`) and the ladder (`abort.rs:269`) all assert a `Call::Snapshot`, but nothing asserts the turn-settle site: `turn.rs` and `stop.rs` have no unit tests mentioning `Snapshot` (`grep`), and `tests/turns.rs` never inspects backend calls. Deleting `self.snapshot(session).await;` at `turn.rs:319` leaves `cargo test` and `scripts/verify.sh` green — one of HS-042's five acceptance bullets would be silently unmet. Smallest fix: extend `tests/turns.rs::the_turn_settles_the_session_and_moves_last_active_at` to assert the stub recorded `Call::Snapshot`, and add one `fast_stub().failing_snapshot()` test asserting the row still reaches `idle` with no `status_reason` (the ADR's "a git failure must not turn a finished turn into an `error` row" is also unasserted).

**Finding: P1 — the property the ADR calls "load-bearing" is untested.** `snapshot.rs:13-17` and `ADR-0049:13-16` both argue that `read-tree HEAD` (`snapshot.rs:122`) is what makes a tracked file the agent *deleted* a staged deletion. No test deletes a tracked file: `tests/snapshot.rs:71-104` and `case_snapshot` only modify one file and add one. Removing the `read-tree` call keeps all three tests and the gate green, while producing a wip ref that silently restores files the agent deleted — the wrong direction for a data-safety story. Smallest fix: in `a_snapshot_keeps_uncommitted_work_without_moving_the_branch_or_the_index`, `remove_file(tracked.txt)` and assert `git cat-file -e refs/holosmith/wip/<id>:tracked.txt` fails (and, in the clean-tree test, that deleting a tracked file alone still writes a ref).

**Finding: P2 — the identity and message the acceptance names are never read back.** Acceptance 1 names "commit-tree with the holosmith identity"; nothing asserts the author or the `holosmith snapshot` subject, in `tests/snapshot.rs` or the gate, which is why the unused `IDENTITY` const (above) could drift unnoticed. Smallest fix: `assert_eq!(git(&project, &["log","-1","--format=%an <%ae>|%s", &snapshot::reference(id)]), "holosmith <holosmith@localhost>|holosmith snapshot")`.

**Finding: P2 — the mechanism differs from spec item 9 and the deviation is not recorded.** `HolosmithSpec.md:163` says "A snapshot script in the image, run with `docker exec s_<id> hs-snapshot`"; this implementation runs git in-process on the host (`docker.rs:1021-1035`) and ships the container mode only as the CLI subcommand (`cli.rs:52-58,85-89`). The subcommand is spec-sanctioned (`HolosmithSpec.md:358`), and host-side execution is strictly more capable (it works when the container is dead or when the session is already `stopped`), but `ADR-0049` never says so — it lists the container mode and three call sites as if the container does the work. That is the kind of unrecorded substitution the repo's "authoritative specification" rule is meant to prevent. Smallest fix: one paragraph in `ADR-0049` under "The temporary index is what keeps the promise" saying the host runs it and why, and noting that `docker exec <path>/holosmith container snapshot` is the manual equivalent.

---

## Merge verdict

**OK with notes.** I found no functional defect in `snapshot.rs`, `docker.rs::snapshot`, `turn.rs` or `stop.rs`: the four git commands, the index isolation, the remove-on-empty-tree rule and the three call sites all do what item 9 and the HS-042 acceptance say, and every pre-existing test whose exact call list changed was updated correctly. The two P1s are test-only additions to a data-safety story (the `agent_settled` site and the deletion case); I would take those plus the `IDENTITY`/duplication cleanup before this ships, and the rest are report-only.

## Could not verify without running

- `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`: not executed. In particular I could not confirm that `tests/snapshot.rs` and the rewritten `docker.rs` assertion (`docker.rs:1264-1275`) are rustfmt-clean, nor that `crates/holosmith/Cargo.toml`'s `unwrap_used = "warn"` gate is satisfied across the new test file.
- `scripts/verify.sh` with `case_snapshot` (and the assertion-count claim): the case needs `dist/<host_target>/holosmith` from `scripts/build.sh`. I could not run it, so I cannot confirm the gate is green, or that the new assertion count matches any recorded baseline.
- The mutant argument in the two P1 findings (removing `read-tree`/`snapshot` leaves the suite green) is a reading of the tests, not a run of them.
- Whether two snapshots for one session actually overlap in practice (the per-session index collision) — the window is real in the code (`stop.rs:54` vs `turn.rs:319`), but its reachability depends on timing I cannot measure here.
- That `git rev-parse --verify --quiet <missing-ref>` exits non-zero on this machine's git (2.39.5) so the `Removed` path is not an error — that is a behaviour claim I read rather than exercised.
---

## Resolution

| Finding | Resolution |
| --- | --- |
| **Standards P2** — git plumbing duplicated from `worktree.rs` | **Report-only.** `snapshot::run` carries `GIT_INDEX_FILE`, which `worktree::git_run` does not, and `SnapshotError` has an index variant `GitError` does not. A shared `git` module is a later refactor; no correctness or security difference. |
| **Standards P2** — `tests/snapshot.rs` re-implements the harness | **Report-only.** The test needs a linked worktree and its own failure assertions; moving it onto `support` is a test-harness cleanup, not a change to the shipped behaviour. The identity write the reviewer flagged is removed by the next row. |
| **Standards P2** — `IDENTITY` dead / literals duplicated | **Fixed.** `take_inner` builds `-c name=…` and `-c email=…` from `IDENTITY` and uses `reference(id)` for the ref. The test asserts `holosmith <holosmith@localhost>|holosmith snapshot`. |
| **Standards P2** — `commit_args.clear()` no-op | **Fixed.** The temporary `mut`/`clear()` is gone. |
| **Standards P2** — a filesystem failure is reported as a git command | **Fixed.** `SnapshotError::Index { path, detail }` says the temporary index could not be made. |
| **Standards P2** — stale eviction comment | **Fixed.** It now says `Delete` is data loss and the `Snapshot` before the `Stop` is HS-042 keeping the work. |
| **Standards P2** — `docs/build.md` misclassifies `case_snapshot` | **Fixed.** It is out of the per-boot list and has its own described clause. |
| **Standards P2** — `stop.rs`'s flag invariant is not reconciled | **Fixed.** The module doc now says the snapshot precedes the flag because spec item 9 orders it that way, not because the host worktree would vanish. |
| **Standards P2** — `HS_SESSION_ID` joined into a path unchecked | **Fixed.** `commands/container.rs` refuses anything but 16 lowercase hex characters. |
| **Standards P2** — two snapshots of one session can collide on one index | **Fixed.** The backend's index name includes the process's command counter (`wip-<n>.index`), so a settle and a stop cannot delete each other's index. |
| **Spec P1** — the `agent_settled` site is asserted nowhere | **Fixed.** `tests/turns.rs::the_turn_settles_the_session_and_moves_last_active_at` asserts `Call::Snapshot`; `a_snapshot_that_fails_does_not_turn_a_finished_turn_into_an_error` asserts the row still reaches `idle` with no reason. |
| **Spec P1** — `read-tree HEAD`'s deletion property is untested | **Fixed.** `a_deleted_tracked_file_is_a_deletion_in_the_snapshot` deletes a tracked file, asserts `ref:tracked.txt` does not exist, and asserts the commit still exists. |
| **Spec P2** — identity and message are not read back | **Fixed.** The snapshot test asserts the author and subject. |
| **Spec P2** — host-side execution differs from spec item 9 and is unrecorded | **Fixed.** ADR-0049 now says the host runs the module, why, and that `holosmith container snapshot` is the manual/container equivalent. |
