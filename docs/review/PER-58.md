# PER-58 — HS-026 worktree refusal rules

**Fixed point:** `bae8b66` (the handoff commit before this story). **Change reviewed:** `bae8b66...18e4695`.
**Verdicts:** Standards — PASS with findings (1 P2, 5 P3); Spec — PASS with findings (1 P2, 3 P3); merge verdict "OK with notes".

Saved verbatim from the `reviewer` child, then triaged below.

---

## Review

**Severity scale used** (the brief asks P1/P2/P3, so P3 is added to the skill's P0/P1/P2): **P1** = fix before this merges · **P2** = fix before release / before the next dependent story · **P3** = report-only nit or judgement call.

Fixed point `bae8b66` → `18e4695`, diff read in full from `/tmp/holosmith/reviews/PER-58-diff.txt`, every touched file then read in the repository.

---

## Standards

**Correct.** The three new codes are constants in one module with a constructor each and a doc comment citing HS-026 (`crates/holosmith/src/http/error.rs:50-58`, `262-289`), which is exactly ADR-0008's "a new code is a constant plus the story that returns it, and that story's test asserts it". `repository_of` removes the duplicated `projects_root.join(project)` (`src/worktree.rs:262`, used by `Worktree::of` at `:81` and by `DockerBackend::refuse_project` at `docker.rs:58-62`) — a genuine consolidation, not a second naming rule. The refusals are read off the runtime's worker (`sessions.rs:299-313`) per ADR-0029; the create's lock scope still holds no `.await`; ADR-0030, the `docs/stories/README.md` count ("Twenty-nine" → "Thirty" plus the ADR-0030 line), `GLOSSARY.md:344-348`, `docs/build.md` and the `verify.sh`/`node:test` cases are all updated in the same change. No smell in the skill's baseline fires beyond those named below.

**Finding P2 — a second log-and-500 pattern, at the wrong level, for a repository failure on the create.** `crates/holosmith/src/http/sessions.rs:306-311` and `:326-331` hand-roll `tracing::warn!(…)` followed by `ApiError::internal()`. The repo's one pattern for this is `internal(operation, error)` (`src/http/error.rs:382-390`), which logs **both the operation and the error at `error` level**, and it is what every other create-path failure uses (`sessions.rs:492`, `:501`, `:527`, `:529`). ADR-0012, Consequences, is explicit: "A repository failure during a create answers 500 `internal` … and the operation and the error go to the log at `error` level." The two new sites log at `warn` and record no `operation`, so a 500 the operator sees has one fewer word in the log than every other 500. PROMPT.md rule 2 ("a second pattern for something that already has one") makes this a documented breach rather than a taste call. Smallest fix: replace both bodies with `Some(internal("refuse", error))` / `internal("refuse", error)` and delete the two `warn!` blocks.
*Evidence:* `sessions.rs:306-311`, `:326-331`; `error.rs:382-390`; `docs/adr/0012-…md` Consequences bullet 1; `sessions.rs:492,501,527,529`.

**Finding P3 — the new backend operation is an inherent enum method, and "refuse" now means three things.** Every other backend operation is a `SessionBackend` trait method (`backend/mod.rs:213-224`) with `Backends` delegating one `match` arm each (`:321+`); this one is `Backends::refuse` (inherent, `:310-318`) plus a differently-named `DockerBackend::refuse_project` (`docker.rs:58`). `docker.rs` already has a private `fn refuse(operation: &str) -> BackendError` (`:66`) meaning "this backend does not implement that operation", so the file now uses *refuse* for both a policy refusal and an unsupported-operation refusal, and the diff's `use` list in `docker.rs` writes `crate::worktree::refuse` / `crate::worktree::Refusal` fully qualified while the same file imports `crate::worktree::Worktree` at the top (`docker.rs:26`). Judgement call: the enum `match` is exhaustive, so no backend can silently default, and ADR-0030 argues the placement. Smallest fix: one name for both levels (`Backends::refuse_project` or `DockerBackend::refuse`) and an import line.

**Finding P3 — the module header now contradicts itself.** `src/worktree.rs:3` still says "Spec item 1 names five host commands, and this module is all five and nothing else", while the paragraph this diff inserted at `:16-19` says the module "also holds the readings that say a repository must not be mounted at all". Both statements are in the same doc block. Smallest fix: amend line 3 to name the five commands *and* the refusal readings.

**Finding P3 — a doc comment says the opposite of what the code does.** `error.rs:258`: "Three sentences rather than one per code, because the operator's next step depends on which hole was found" — read literally, that says *not* one per code; the test's own comment (`error.rs:555-558`) says "a sentence each … a shared sentence would make the codes indistinguishable". Smallest fix: "A sentence each rather than one shared sentence, because…".

**Finding P3 — `let live` now holds a row count, not the live count.** `crates/holosmith/tests/provisioning.rs:376`: `let live = { … list(None) … .len() };` The old value was `live_count()`, which is what the name meant; `list(None)` counts every row in every state, so the name now claims something the expression does not compute (a `deleted`/`stopped` row would also be counted). Smallest fix: `let rows` / `let written`.

**Finding P3 — a glossary concept placed outside the section that defines its parent.** `GLOSSARY.md:344-348` puts **Refused project** under "Work and results" (`:339`) between **Worktree** and **Session branch**, while the term it specialises — **Project** — is defined under "Sessions and turns" (`:70`), next to **Projects root** in "Configuration" (`:60`). Defensible (the machine home is `worktree.rs`), so report only; if moved, one cut-and-paste.

**Finding P3 — the diff bans a word it then uses.** The new entry's `_Avoid_: Invalid project, bad repo, unsafe project` (`GLOSSARY.md:348`) lands in the same diff as prose and a test name that use exactly that vocabulary: `src/worktree.rs:269` '"saying "safe" about it would be a lie' and `tests/worktree.rs:679` `fn a_repository_git_cannot_read_is_not_called_safe()`. The glossary is above ADRs in PROMPT.md rule 2's order, so the new term should own the vocabulary from the moment it lands. Smallest fix: "not called mountable" / `…_is_not_treated_as_mountable()`.

**Standards verdict: PASS with findings** (one P2, five P3; none blocking).

---

## Spec

**Correct, against all five acceptance lines.** (1) `git_not_directory`, (2) `submodules_present`, (3) `worktree_config_enabled` each answer `422` with `application/json` and their own constant (`error.rs:50-58`, `262-289`), asserted per code on the HTTP path (`tests/worktree.rs:715-745`) and from the shipped artifact (`scripts/verify.sh:1605-1690`). (4) The checks run in `create` before `slot_for` writes anything, so no row, no `.worktrees/<id>` and no branch: `sessions.rs:270` precedes `:271`'s insert, and the test asserts the empty list, the absent `.worktrees` directory and one registration per project (`tests/worktree.rs:747-770`). (5) The page prints the sentence alone for those three codes and keeps `code: message` for everything else (`web/src/transcript.ts:378-393`, `web/src/main.ts:348-352`), pinned by `web/test/transcript.test.ts:487-508`. The demo's distinctness is pinned twice (`error.rs:555-577`, `worktree.rs:731-737`). The two deliberate scope decisions — refusing per *create* rather than per provisioner, and turning HS-025's "accepted into an `error` row" into a 422 — are recorded in ADR-0030 and are correctly reflected in the rewritten `tests/provisioning.rs:340-386`; and the end-to-end Docker case the rewritten test points at does exist (`tests/worktree.rs:435-464`), so no coverage was dropped.

**Finding P2 — a resume mounts a project the create would refuse.** `POST /api/sessions/{id}/resume` (`sessions.rs:787-814`) calls `state.provisioner().spawn(…)` with no refusal read; the provisioner runs all four steps unconditionally (`provisioner.rs:59-66`), and the worktree step's own test resumes a Docker session through them (`tests/worktree.rs:501-570`). So a session created while the project was plain, then stopped, is re-provisioned after the project gains `.gitmodules` / `extensions.worktreeConfig` — the exact hole the story exists to close ("**so that** I do not discover the hole after an agent exploits it", `docs/stories/phase-1b-docker-backend.md:47`). Spec item 1 says only "Refused with 422 at session creation" (`HolosmithSpec.md:55`), so this is a gap in the guard rather than in its reading, and ADR-0030 does not mention resume at all. It is latent today because the network step refuses before any mount exists (HS-027 supplies the mounts, and it is blocked by this story) — hence P2 rather than P1. Smallest fix at this point: one paragraph in ADR-0030 recording that the rule is create-only *by decision*, or a `refuse` read in `resume` answered with the project's code before `apply_trigger`.

**Finding P3 — `git config` without `--local` lets a scope decide that the repository does not own.** `worktree.rs:309-314` runs `git config --type=bool --get extensions.worktreeConfig`, and a plain `--get` reads system + global + local (and `-c`) merged, so a `extensions.worktreeConfig = true` in the service user's `~/.gitconfig` or `/etc/gitconfig` would refuse *every* project with `worktree_config_enabled` — and `extensions.*` is a repository-format key that git itself only honours from the repository's own config. ADR-0030's "what is read" section describes the reading as a fact about the repository. A supervisor can settle the empirical half cheaply: in a throwaway repo, `git -c extensions.worktreeConfig=true config --type=bool --get extensions.worktreeConfig` — if that prints `true`, the false refusal is reachable. Smallest fix: add `--local`.

**Finding P3 — `submodules_present` fires on more than the acceptance line names.** `worktree.rs:303` refuses on `.gitmodules` alone, while the acceptance line is "when submodules exist (their hooks live under `.git/modules`)" (`docs/stories/phase-1b-docker-backend.md:53`) and spec item 1 gives the same parenthetical (`HolosmithSpec.md:55`). A repository that merely *ships* `.gitmodules` with nothing initialised can never be used, even though no `.git/modules` exists. ADR-0030 argues the case (the mount is writable, so an initialised submodule could appear after the check), and that argument is itself unproven until HS-027 fixes the mount table (spec item 1 mounts `<project>/.git` and not `<project>`; `.gitmodules` reaches the agent through `/work`, which is the worktree). Either cite the ADR's reason in the story/ADR line the finding is judged against, or narrow to `.git/modules`. Report only.

**Finding P3 — coverage note.** `Refusal::Unreadable` is proven at the value level (`tests/worktree.rs:679-701`) but nothing proves the ADR's claim that it reaches a client as `500 internal` with git's words logged but not leaked. One HTTP case (`post_create_for` over a repo with `worktreeConfig = maybe` → `422`-vs-`500`, code `internal`) would close it; the assertion is cheap because `support::git` already exists.

**Spec verdict: PASS with findings** (one P2, three P3).

---

### Verified by reading

| Claim | Where |
|---|---|
| Three codes, three constants, three constructors, no sentence repeats its code | `src/http/error.rs:50-58`, `262-289`, test `:555-577` |
| `.git` → `git_not_directory`; `.gitmodules` **or** `.git/modules` → `submodules_present`; key `true`/`on` → `worktree_config_enabled`, `false`/`off` → allowed | `src/worktree.rs:299-322` |
| Checks run before the row exists and before any `worktree add` | `sessions.rs:270` → `:271`; `provisioner.rs:59` spawns only on `inserted` (`sessions.rs:274-283`) |
| A refused create writes nothing (no row, no `.worktrees`, registration unchanged) | `tests/worktree.rs:747-770`; `scripts/verify.sh:1673-1686` |
| Refusal answered after the idempotency lookup, before the cap, so no eviction | `sessions.rs:490-498`; tests `worktree.rs:772-802`, `:804-825` |
| The stub refuses nothing (gate + stub tests need no projects root) | `backend/mod.rs:314-317`; test `worktree.rs:827-841` |
| The read is off the runtime worker, and a panic is `500`, not a refusal | `sessions.rs:296-313` (ADR-0029) |
| Page prints the sentence alone for exactly these three codes | `web/src/transcript.ts:378-393`, `web/src/main.ts:348-352`, `web/test/transcript.test.ts:487-508` |
| HS-025's behaviour change is recorded, not papered over | `docs/adr/0030-…md` "The refusal is the create's…"; `tests/provisioning.rs:340-386` |
| No coverage lost by the rewritten provisioning test (safe-project 202 → `error` at the network step still covered) | `tests/worktree.rs:435-464` |
| Direct backend provision of a missing repository still answers the step + git's words (ADR-0030's claim) | `tests/worktree.rs:361-384` |
| A resume bypasses the refusal (P2 above) | `sessions.rs:787-814` → `provisioner.rs:59-66`; `tests/worktree.rs:501-570` |

### Could not verify without running

1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cd web && npm test` — the brief reports these green; I re-read every changed hunk but cannot re-run them.
2. `scripts/verify.sh` end to end: that `case_worktree_refusal` passes, that it adds the 11 assertions it claims (case count 41 → 42, 253 total), and that leaving its own server running (`scripts/verify.sh:1687-1690`) does not disturb `case_docker_preflight`, which follows it in `serve_cases` (`:316-319`) despite `docs/build.md:186-188` telling self-booting cases to sit at the end.
3. `git config --type=bool --get <unset key>` → exit 1 and `… = maybe` → exit 128 on this host's git (the `Unreadable` branch's premise, `worktree.rs:316-321`).
4. The P3 false-refusal repro above (`git -c extensions.worktreeConfig=true config --type=bool --get extensions.worktreeConfig` inside a clean repo).
5. The demo from the shipped artifact (three 422s with their codes and sentences, one 202 for a safe repository) — the brief reports it was run; I only read the code paths it exercises.

**Merge verdict: OK with notes** — no P1 on either axis; the P2 on resume should be decided (or at minimum recorded in ADR-0030) before HS-027 mounts anything, and the `internal(…)` log-pattern P2 is a two-line fix.
---

## Resolution

Every finding was re-checked against the code before it was triaged. `internal(operation, error)`
(`src/http/error.rs:382`) was read first, since the standards P2 rests on it.

| Finding | Triage | How / why |
| --- | --- | --- |
| **Standards P2** — hand-rolled `warn!` + `ApiError::internal()` at `sessions.rs:306-311` and `:326-331`, where the repo's pattern is `internal(operation, error)` | **Fixed** | Both sites are now `internal("refuse_project", error)`: the `JoinError` and `Refusal::Unreadable` each go through the one helper, which logs the operation and the error at `error` level (ADR-0012's consequence). The comment on `Unreadable` says why git's words stop at the log. |
| **Standards P3** — "refuse" means three things; `Backends::refuse` vs `DockerBackend::refuse_project` | **Fixed** | `Backends::refuse_project` now matches `DockerBackend::refuse_project`, so both levels spell one name; `docker.rs` imports `Refusal`, `refuse` and `repository_of` instead of writing the paths out. The pre-existing private `DockerBackend::refuse(operation)` is kept: it means "this backend does not implement that operation" (`BackendError::unsupported`), which is a different question from a policy refusal, and renaming it would churn ten call sites for a word this story did not introduce. |
| **Standards P3** — the module header says "all five and nothing else" while the new paragraph says it also holds the refusal readings | **Fixed** | `src/worktree.rs:3` now says "five host commands; this module is those five, the naming rule they share, and since HS-026 the read that says a repository must not be mounted at all". |
| **Standards P3** — `error.rs:258` "Three sentences rather than one per code" reads as the opposite of the code | **Fixed** | "A sentence each rather than one shared sentence, because the operator's next step depends on which hole was found". |
| **Standards P3** — `let live` in `tests/provisioning.rs` now counts every row | **Fixed** | Renamed to `let rows`, which is what `list(None).len()` computes. |
| **Standards P3** — **Refused project** sits under "Work and results", while its parent **Project** is under "Sessions and turns" | **Dismissed** | The entry's machine home is `crates/holosmith/src/worktree.rs`, and its neighbour is **Worktree**, which is what the operator compares it with ("this project cannot be a session's worktree"). `GLOSSARY.md` has no rule that a term sits beside its hypernym, and moving it would put the machine name a section away from the concept it names. |
| **Standards P3** — the new entry bans "unsafe project" while the diff says "safe" and names a test `…_not_called_safe` | **Fixed** | The prose is now "saying it may be mounted would be a lie" and the test is `a_repository_git_cannot_read_is_not_treated_as_mountable`. The banned vocabulary is the glossary's from the moment the entry lands (PROMPT.md rule 2). |
| **Spec P2** — a resume mounts a project the create would refuse | **Fixed** | `resume` now reads the same refusals, after the state check and before the cap, and answers the project's three codes with the row untouched. New test `a_resume_refuses_a_project_the_create_would_have_refused` (stopped row + `.gitmodules` → `422 submodules_present`, still `stopped`), and ADR-0030 has a "A resume asks the same question" section saying why a create-only guard was a hole. |
| **Spec P3** — `git config` without `--local` reads every scope, so a global `extensions.worktreeConfig` would refuse every project | **Fixed** | `--local` added, and the premise measured before the fix: with `GIT_CONFIG_GLOBAL` naming a file that sets the key, the read answers `true` (exit 0) without `--local` and exits 1 with it. The spelling is pinned by `the_worktree_config_read_is_local_and_typed` over the new `CONFIG_READ` constant; a behavioural test would have to set this process's global git config, which edition 2024 makes `unsafe` (and this crate denies `unsafe`). |
| **Spec P3** — `submodules_present` fires on `.gitmodules` alone, which is more than "submodules exist" | **Dismissed, and the ADR now argues it** | The declaration is enough because it reaches the agent: `.gitmodules` is checked out into the session's **worktree** (`/work`), so `git submodule update --init` writes the submodule's gitdir — hooks included — under the **mounted** `<project>/.git/modules`, which the `hooks`/`config` read-only mounts do not cover. The check is therefore "can this repository carry those hooks", and ADR-0030 now spells that out rather than asserting it. Narrowing to `.git/modules` would reopen the hole on any repository whose submodules have not been initialised yet. |
| **Spec P3** — nothing proves `Refusal::Unreadable` reaches a client as `500 internal` with git's words only in the log | **Fixed** | `a_repository_git_cannot_read_answers_500_with_gits_words_in_the_log_only`: `extensions.worktreeConfig = maybe` → `500`, `application/json`, code `internal`, no "boolean" in the body, and no row written. |
| **"Could not verify" #2** — the new self-booting case sits before `case_docker_preflight`, against `docs/build.md`'s "self-booting cases go at the end" | **Fixed** | `case_worktree_refusal` is now the last entry in `serve_cases`, after `case_docker_preflight`; the CORS sweep still runs after every case and covers its saved answers. |

Re-run after the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
(226 lib + 22 `tests/worktree.rs` among the integration suites), `cd web && npm test` (28), and
`scripts/verify.sh` (42 cases, 253 assertions) — all green on a clean tree. The demo was re-run from the
rebuilt artifact.
