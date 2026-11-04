# Review — PER-57 (HS-025, worktree provisioning)

- **Issue:** PER-57, story HS-025 — "Worktree provisioning", from
  `docs/stories/phase-1b-docker-backend.md`.
- **Fixed point:** `fd69cf1` (the commit before this story's work). Diff artifact:
  `/tmp/holosmith/reviews/PER-57-diff.txt` — 17 files, 2034 lines, regenerated after the fixes below.
- **Spec sources:** `HolosmithSpec.md` item 1 (the five host commands, the `.git` mount) and item 12
  (`ahead` in `GET /api/sessions/:id/changes`), the Data model section, the HS-025 slice, the Linear body,
  the plan in `/tmp/holosmith/plans/PER-57.md`, and ADR-0029 (this change's own) plus ADR-0015, ADR-0019,
  ADR-0022 and ADR-0023.
- **Standards sources:** `AGENTS.md`, `GLOSSARY.md`, `docs/build.md`, `docs/stories/README.md`, the ADRs,
  and `RESUME.md`'s landmine list.
- **Verdicts:** Standards — **PASS with findings**, one P1 and four P2/P3. Spec — **PASS with findings**,
  one P2 (the demo). The (later) Resolution records that the P1's fix was verified by reproducing the
  primitive, and that the adopted-worktree finding was a gap in the plan, not in the code.

The review below is the reviewer's, verbatim; every finding is answered in the Resolution at the end.

---

## Standards

Sources read: `AGENTS.md`, `docs/build.md`, `GLOSSARY.md`, ADR-0015/0019/0023/0028/0029, plus the repo habits the brief names. Smell baseline applied as judgement calls only.

**P1 — `case_worktree`'s SKIP path fails the gate, and three sentences say it passes.**
`scripts/verify.sh:1517` opens the case with `stop_serve` (which sets `serve_pid=""`), and the python3 guard at `:1524-1527` returns before `start_serve` at `:1537`:

```sh
  stop_serve
  ...
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP %s: no python3 ...\n' "$name"
    return 0
```

`phase_serve` then runs `case_docker_preflight` (boots nothing), `case_no_cors_header`, and `stop_serve` at `:1733-1734`. `stop_serve` is:

```sh
  kill -TERM "$serve_pid" 2>/dev/null ||
    fail "serve/sigterm" "could not signal the server (pid $serve_pid)"
```
(`:1685-1688`). With `serve_pid` empty that is `kill -TERM ""`, which exits non-zero, so `fail` exits 1: the run ends `FAIL serve/sigterm`. Three places promise otherwise: `verify.sh:21-23` ("prints one SKIP line and passes without python3"), `:1481-1483`, and `docs/build.md:268` ("Without it that case prints one `SKIP` line and the run passes"). The SKIP also silently drops the only remaining SIGTERM-exit-0 assertion, so it is not even honest about what it skipped. *Why it matters:* a blessed configuration (docs/build.md marks python3 **Optional**) turns the one-command gate red at the last step, with a failure that blames the wrong thing. *Smallest fix:* move the `command -v python3` guard to the first line of `case_worktree`, above `stop_serve`.

**P2 — `docs/build.md:292` attributes the git call to the wrong component.**
"The provisioner makes the worktree with `git worktree add -b …`" — but ADR-0029's title is "The **backend** makes it; the provisioner writes the row", and the code is `DockerBackend::provision_worktree` (`backend/docker.rs:113`) running `Worktree::ensure`. Also "records both the path and that commit" is fine, but the sentence reads as if the git commands live in `provisioner.rs`, where they do not. *Fix:* "The backend makes the worktree …; the provisioner writes both the path and that commit to the row."

**P2 — `docs/build.md:171` states a status the case does not assert.**
"…and then a `DELETE ?force=true` to `404` over the worktree that is already absent." The case asserts `expect_status "${name}_delete" … 202` (`verify.sh:1592`) and *then* `wait_for_gone` (a GET polling for 404, `:1594`). The DELETE answers 202, and its object is the session, not the worktree. A later maintainer reconciling code with doc would "fix" the case to expect 404.

**P2 — dead `Step::Worktree` arm in `not_yet`, with a message naming no story and contradicting the file's own doc.**
`backend/docker.rs:65-67`:
```rust
            Step::Worktree => BackendError::unsupported_step(
                step,
                "the docker backend does not provision the worktree yet",
            ),
```
This arm is unreachable: `provision` matches `Step::Worktree => self.provision_worktree(session)` first (`:141-146`). It is also the only sentence in `not_yet` with no `(HS-0xx)`, and it contradicts `docker.rs:5` ("**The worktree step is real from HS-025 on**"). The tell is in the test: `the_steps_this_build_does_not_provision_refuse_and_name_their_story` (`:232`) loops over Network/Container/Pi only. *Fix:* make the arm say it is unreachable, or restrict `not_yet` to the three steps. (Baseline smell: possible **Speculative Generality** — but this is unreachable code, not abstraction-for-later.)

**P2 — `undo` of the worktree step is destructive on the resume path the ADR itself names.**
`provisioner.rs:86-99` records the step, then on any later failure calls `self.undo(&session, &completed)` (`:99`, `:165-166`), and `undo(Step::Worktree)` is `remove_worktree` → `Worktree::remove` (`worktree.rs:154-166`), which runs `git worktree remove --force` **and `git branch -D agent/<id>`**. Resume spawns the provisioner (`http/sessions.rs:738-750`), `ensure` deliberately *adopts* an existing worktree and keeps its commits (ADR-0029 §"create or adopt", `worktree.rs:96-118`), so a resumed session whose later step fails loses the branch and every commit on it. It loses nothing *today* only because the network step is the first failure point and the branch was just re-created — i.e. the path becomes data-lossy at HS-027, and no test covers "resume a session with commits, then fail". ADR-0029 argues the adopt half and is silent on the undo half, so this is **the plan being incomplete**, not the code contradicting it.

**P3 — a cross-reference the codebase does not keep.** `worktree.rs:51-52`: "the branch name and the directory are derived from the session id in exactly one place (`Session`'s own doc spells the same rule)". Nothing in `crates/holosmith/src/sessions/` spells `agent/<id>` or `.worktrees/<id>` (`Session`'s fields say only "`''` until the provisioner runs"). The rule is actually spelled at `backend/mod.rs:37` and `GLOSSARY.md:342`.

**P3 — `Worktree::ahead` ships with no production caller.** Only `tests/worktree.rs:213,220` call it. `pub`, so `dead_code` cannot warn. Plan-sanctioned (ADR-0029 says its publisher is HS-061), so I am not calling it scope creep — only noting that the story's second command is unreachable from the binary.

**P3 — smaller doc/consistency nits.** `GLOSSARY.md:342` writes `src/worktree.rs` where `GLOSSARY.md:105` writes `crates/holosmith/src/turn.rs`. `docs/build.md:182-184` says an own-boot case "takes the last place in the array"; `case_worktree` is added second-to-last (`case_docker_preflight` follows), and `docs/build.md:165` still calls `case_create_at_cap_refused` "the last case" — that clause predates the diff, but the diff edits this exact sentence. `tests/provisioning.rs:346-364` now depends on the host's `/srv/holosmith/projects/holosmith` not being a repository.

**Standards verdict: PASS with findings.** Shipped Rust is clean: idempotence, canonicalisation, "already gone is success", blocking-off-the-worker, ADR-0019 separation and the poison-tolerant `record` all match their comments and are tested. The one P1 is confined to `scripts/verify.sh`'s no-python3 path plus the three sentences that promise it passes; if the convention is "any P1 blocks", read this as BLOCK — the fix is two lines.

## Spec

Measured against HS-025's acceptance list (`docs/stories/phase-1b-docker-backend.md:29-42`), spec item 1 (`HolosmithSpec.md:34-46`) and item 12 (`:223`).

**P2 — the story's demo cannot be demonstrated from the artifact, and the plan never says so.**
HS-025's demo (`phase-1b-docker-backend.md:34`): *"create a session, see `.worktrees/<id>` with branch `agent/<id>`, commit in it, see `ahead` increment, delete and see it gone."* Since HS-027's network step is what refuses, `provisioner.rs:99` undoes the worktree moments after creating it — `case_worktree` itself asserts `the refused session left its worktree at …` (i.e. that it is **gone**). So the artifact can only prove the *row*, never a live worktree with commits on it, and `ahead` has no caller at all. `tests/worktree.rs` covers the behaviour at unit level (`ahead_counts_the_commits_the_branch_has_that_the_base_does_not`, `:203-232`), which is the right substitute, but the demo is half-demonstrable and no plan section or ADR line records that. *Why it matters:* "demo" is the story's acceptance of *observability*; the next reader will assume it was shown.

**Nothing else in the acceptance list is missing or wrong.**
- Command 1 — `worktree.rs:123-131` runs `worktree add -b agent/<id> <projects>/.worktrees/<id> <base|HEAD>`; the path and commit reach the row via `Provisioned::Worktree` → `provisioner.rs:119-139` → `SessionRepository::set_worktree` (`sessions/repository.rs:278`). ✓
- Command 2 — `worktree.rs:134-140` is exactly `rev-list --count <base_commit>..agent/<id>`. ✓
- Command 3 — `worktree.rs:154-166` runs `remove --force`, `branch -D`, `prune`, and "already gone" is handled by asking *before* running (`registered()`, `branch_exists()`), with a missing project repository returning `Ok`. ✓
- Command 4 (startup prune) — not implemented; correctly attributed to HS-044 and a stated plan non-goal, so **deferred by name**, not a defect.
- "No fetch step anywhere" — no `fetch` invocation exists; only the doc sentence forbidding one. ✓
- Item 12's `ahead` is deferred to HS-061 by name in ADR-0029 and both docs; the row carries `base_commit` for it. ✓
- Data model: `worktree` stays `TEXT NOT NULL` (`''` until written — asserted in `tests/provisioning.rs:371-372`), `base_commit` nullable, **no schema change**, consistent with "any schema change" being a non-goal. ✓

**Spec verdict: PASS with findings** (one P2, demo-only).

## Verified by reading

| Acceptance / claim | Where | Result |
|---|---|---|
| `git worktree add -b agent/<id> … HEAD` | `worktree.rs:123-131` | ✓ |
| path + `base_commit` recorded on the row | `docker.rs:113-138`, `provisioner.rs:119-139`, `repository.rs:278-286` | ✓ |
| `ahead` = `rev-list --count base..branch` | `worktree.rs:134-140` | ✓ (no production caller) |
| `remove --force` + `branch -D` + `prune` | `worktree.rs:154-166` | ✓ |
| missing worktree / branch / repository is not an error | `worktree.rs:155`, `:158-163`; tests `worktree.rs:300-330` | ✓ |
| no fetch, no remote | grep of `worktree.rs`/`docker.rs` | ✓ |
| startup prune | — | deferred to HS-044 by name |
| idempotent adopt keeps the row's base over the branch head | `worktree.rs:96-121`; test `worktree.rs:130-175` | ✓ |
| ADR-0019: backend cannot write the row | `Provisioned` in `backend/mod.rs:156-181`; `provisioner::record` | ✓ |
| ADR-0023: `delete` answers `Ok` for nothing-provisioned | `docker.rs:185-192`; test `docker.rs:298-307` | ✓ |
| `Backends::for_run(backend, projects_root)`; no `Default` caller left | `backend/mod.rs:267`; grep finds none | ✓ |
| every story adds ≥1 `case_*` | `verify.sh:317` | ✓ |
| ADR count and link | `docs/stories/README.md:117`, `:147` | ✓ (0029 file exists) |
| `projects_root` absolute / `validate_project` claims | `config/mod.rs:716`, `http/sessions.rs:160` | ✓ |

## Could not verify without running

1. **The P1 itself.** `. scripts/verify.sh` on a machine with python3 stripped from `PATH`: expect `FAIL serve/sigterm`. Also confirm the primitive: `bash -c 'set -euo pipefail; kill -TERM ""; echo reached'`.
2. `bash -c 'kill -TERM ""; echo $?'` — the exact exit status of the empty-pid kill.
3. `cargo test` + `cargo clippy --all-targets -- -D warnings` (the gate's first three phases); letter-chains at `worktree.rs:184-186` need edition 2024, which the workspace declares.
4. `scripts/verify.sh` end-to-end — `case_worktree` in particular (it is the first gate case that needs a real repository, a fake `_ping` socket and `min_free_gb = 1`).
5. **`git worktree list --porcelain` path quoting.** `registered()` (`worktree.rs:173-191`) strips the literal prefix `"worktree "` and compares `Path`s. If git C-quotes an entry whose path contains a space or a non-ASCII byte (i.e. a `projects_root` of `/home/me/My Projects`), no comparison can match, `ensure` takes the "nothing registered" branch and `git worktree add -b` fails with "a branch named … already exists" on the *second* provision. No `TempDir` in the tests has a space, so the suite cannot catch it. Check with `git -C "/tmp/a b/repo" worktree add "/tmp/a b/wt" && git -C "/tmp/a b/repo" worktree list --porcelain`.
6. That `DELETE ?force=true` on an `error` session really answers `202` in the shipped build (the case's `expect_status` assumes it; I read the flow, did not run it).
7. ADR-0029's claim that a `JoinError` is reachable-and-reported: no test provokes a panicking `spawn_blocking` task.

Merge verdict: **OK with notes** — no shipped-code defect found; fix the `case_worktree` SKIP ordering (P1) and the three doc sentences before this reads as done.

---

## Resolution

Every finding below is answered. The fixes are in the commits that carry this file; the gate and the demo
were re-run afterwards.

| Finding | Verdict | What was done |
| --- | --- | --- |
| **P1 (standards)** — `case_worktree`'s SKIP path fails the gate: it called `stop_serve` before the python3 guard, so a skipped case left `phase_serve`'s final `stop_serve` with an empty pid, and `kill -TERM ""` exits 1 (`FAIL serve/sigterm`) | **fixed** | The guard is now the case's first statement, above `stop_serve`, so a skipped case leaves the run's live server alone and the phase still asserts SIGTERM on it. Reproduced the primitive first (`bash -c 'kill -TERM ""'` → exit 1), which is what makes the finding real. |
| **P2 (standards)** — `docs/build.md` said *the provisioner* makes the worktree | **fixed** | Reflowed: the backend runs the command, the provisioner writes the row; the teardown's rule is stated in the same breath, including the adoption exception below. |
| **P2 (standards)** — `docs/build.md` said the case's `DELETE ?force=true` goes "to `404`" | **fixed** | It is a 202 and then a row that answers `404`, and the sentence says both now. |
| **P2 (standards)** — the `Step::Worktree` arm in `not_yet` is unreachable, names no story, and contradicts the file's own doc comment | **fixed** | `not_yet` is gone. `provision` writes the three refusals out per arm, so there is no arm that can never run and the "the worktree step is not provisioned yet" sentence no longer exists to be wrong. |
| **P2 (standards)** — `undo(Step::Worktree)` is destructive on the resume path: an *adopted* worktree is removed (worktree **and** `git branch -D`) when a later step fails, i.e. the agent's commits are deleted to tidy up after a step that made nothing. "The plan being incomplete, not the code contradicting it." | **fixed, and it was real** | `Worktree::ensure` now answers `Ensured { base_commit, created }`, `Provisioned::Worktree` carries that `created` and `Provisioned::undoable()` answers it, and the provisioner records only the steps that *made* something. A delete still removes an adopted worktree — the session is going away and its work was snapshotted (ADR-0023). New test `a_later_step_failure_does_not_take_an_adopted_worktree_back` drives the whole path (create → refused → worktree back with a commit → resume → refused again) and **was proved by mutation**: with `undoable()` forced to `true` it fails on "the adopted worktree was removed when the next step refused". ADR-0029 gained the section "a step that adopted has nothing to undo", and the glossary/README bullets follow. |
| **P3** — `worktree.rs` claimed "`Session`'s own doc spells the same rule" and no doc did | **fixed** | The sentence now names the three places the rule is text: the function, `backend::Step::Worktree` and the glossary. |
| **P3** — `Worktree::ahead` has no production caller | **dismissed, with citation** | Planned and recorded: ADR-0029's "`ahead` is a function, not a column and not an endpoint" names HS-061's `GET /changes` as its publisher, and the story's acceptance is the *computation*. The precedent is ADR-0028's `Complaint` for HS-045's `doctor`. The reviewer agreed this is not scope creep and noted it only. |
| **P3** — `GLOSSARY.md` wrote `src/worktree.rs` where another entry writes `crates/holosmith/src/turn.rs` | **fixed** | The entry now names `crates/holosmith/src/worktree.rs`. |
| **P3** — `docs/build.md` said an own-boot case "takes the last place in the array" and still called `case_create_at_cap_refused` "the last case" | **fixed** | Both sentences now say *a place at the end*, and `case_create_at_cap_refused` is no longer called the last. |
| **P3** — `tests/provisioning.rs`'s Docker test depended on the host's `/srv/holosmith/projects/holosmith` not being a repository | **fixed** | The test builds its own empty projects root in its `TempDir` and passes it to both the config and the backend, so it cannot pass or fail by what the host keeps there. |
| **P2 (spec)** — the story's demo cannot be shown from the artifact (the network step refuses while the worktree is still fresh, so the page can never show `.worktrees/<id>` with commits, and `ahead` has no publisher) and no document said so | **fixed as a record** | ADR-0029 gained "the story's demo is only half showable, and that is recorded rather than papered over", naming what `case_worktree` does assert, what only `tests/worktree.rs` can, and that "HS-025 Done" must not be read as "the demo was performed end to end". The ship comment on PER-57 repeats it. |
| **"Could not verify" #5** — `git worktree list --porcelain` may C-quote a path with a space, which would break `registered()` | **verified, not a defect** | Measured on git 2.43: a projects root with a space and one with non-ASCII bytes both print unquoted in `--porcelain` (and `core.quotepath` does not change it). To keep it from regressing, the adoption test's `TempDir` label now contains a space, so `registered()`'s `canonicalize` comparison is exercised against a path that would break if either half were wrong. |
| **"Could not verify" #4** — the gate end to end, `case_worktree` in particular | **verified** | `scripts/verify.sh` green at 41 cases / 237 assertions, with `case_worktree` running its assertions (three banners in the log: the shared boot, the budget boot and the worktree boot). |
| **"Could not verify" #6** — `DELETE ?force=true` on an `error` session really answers 202 | **verified** | It is in the demo's transcript: `HTTP 202`, then `GET` → `404`. |
| **"Could not verify" #7** — no test provokes a `JoinError` from `spawn_blocking` | **dismissed** | A panicking or cancelled blocking task is not a repository's answer; the arm exists so the failure cannot be mistaken for one, and provoking a panic in production code to test it would need a seam whose only purpose is the test. Recorded as a known untested arm rather than claimed as covered. |
