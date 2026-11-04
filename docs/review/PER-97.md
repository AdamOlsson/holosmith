# PER-97 — HS-065 daily project backup

**Fixed point:** `0a15876` (the HS-061 review's fix commit, which is the state this story started from).
**Change reviewed:** `0a15876...b4d7338` (`ea313e4`, `b4d7338`).
**Verdicts:** Standards — **PASS with findings** (3 P3); Spec — **PASS with findings** (2 P2, 2 P3).
No P0/P1 on either axis, and no behaviour defect the gate could not see — but two of the spec findings
were real holes in *proof* and one was a real race.

Saved verbatim from the `reviewer` child (its cover note about having no shell, and its two notes on the
brief's stale commit list and the artifact's range, are the only parts dropped), then triaged below.

The first `reviewer` child for this story stalled for eleven minutes on a working tree that was changing
under it and was interrupted; this review is from the second, launched against the clean committed state.
That is a lesson for the brief: **commit before you review**, so the child reads a fixed tree.

---

## Review


Fixed point `0a15876`; artifact `/tmp/holosmith/reviews/PER-97-diff.txt` (commits `ea313e4`, `b4d7338`). I read only the committed state via the artifact plus the tree at HEAD. Two notes on scope, both recorded rather than acted on: the brief says the range holds **one** commit (`ea313e4`) while the task says two (`ea313e4`, `b4d7338`); and the artifact contains **no `RESUME.md` hunk**, although `RESUME.md:216-217` and `RESUME.md:495-496` already describe `src/backup.rs` — so either that update is outside the artifact's range or the artifact is incomplete. I did not review the `RESUME.md` text.

---

### Standards — verdict: PASS with findings

**Correct (with evidence).**

- **No SQL, no new dependency, no table.** The diff adds no statement against any table and no `Cargo.toml` change; the only new code is `std` + `tokio::task::spawn_blocking` + `tracing`, all already in use (`crates/holosmith/src/backup.rs:29-31`). `pins/pi.txt` is untouched.
- **The subprocess runs off the runtime worker, and no database guard is held across the `await`.** `sessions.rs:318-319` wraps the whole `take` in `spawn_blocking`, mirroring `refuse_project` (`sessions.rs:359-361`). The call site (`sessions.rs:286`) runs after `slot_for` has returned, and `Slot` carries only owned values (`sessions.rs:385-392`), so no guard can outlive `create_locked` and be held across that `await`.
- **`src/backup.rs` is the `worktree.rs` shape.** Synchronous, `std`-only, `std::process::Command` with `-C`, **no logging inside the module** (grep: no `tracing` in `backup.rs`), and an error type whose `Display` is the operator's sentence (`backup.rs:43-53`) — the failure sentence is printed once, at `sessions.rs:328-338`, where the decision not to block the session also lives. A `JoinError` is treated as "the task did not run", as ADR-0029 requires (`sessions.rs:334-338`).
- **The clock is passed in, not read.** `take` takes `now: i64` (`backup.rs:92-97`); the caller reads `clock::unix_now()` at the seam (`sessions.rs:318`), so the tests name a day instead of racing midnight (ADR-0009). `tests/backup.rs:22-23` pins `2026-11-04` and I checked the constant: `1_793_750_400 = 86_400 × 20_761`, and day 20 761 since the epoch is 2026-11-04. `backup.rs:262-271` also pins the epoch and 2000-02-29, which is the part of `civil_from_days` that is easy to get wrong.
- **The gate gets one case, placed correctly.** `case_backup` exists (`scripts/verify.sh:1791-1853`), is in `serve_cases` (`scripts/verify.sh:324`), boots its own server with its own config and data dir (`:1814-1816`, tag `serve-data-backup` per the `start_serve` rule at `:2121`), and sits in the own-boot tail as `docs/build.md:205-206` requires — nothing after it assumes the first boot's state, because `case_worktree` and `case_worktree_refusal` boot their own too. The `local` splitting at `:1796-1798` is right for `set -u`.
- **A new ADR is registered the way the previous session got wrong.** `docs/stories/README.md:116` moves 33 → 34, and the bullet is **inside the list** at `docs/stories/README.md:152`, before "Gaps in the spec" (`:154`) — not appended after it. `docs/build.md:181-185` and `:206` describe `case_backup` and name ADR-0034; `docs/build.md`'s description matches what the case actually asserts.
- **Vocabulary lands in the glossary with machine names.** `GLOSSARY.md:381-383` gives *Project backup*, an `_Avoid_` line that disambiguates it from the two existing snaps, and names `backup::Backups` / `Backups::take`, which are the real paths (`lib.rs:4`, `backup.rs:88`).
- **The config key was not re-invented.** `project_backup` already exists in the only place ADR-0006 allows it (`config/schema.rs:158-162`, `config/mod.rs:197`, default `false` at `:234`); the story adds no key.
- **Untested surfaces are honest about the shape.** No `unwrap` in `src`; no speculative config key, no marker file, no column — the design's "the name is the state" is what the code does.

#### Findings

- **P3 — `ST-1`: `prune`'s count is computed and thrown away.** `crates/holosmith/src/backup.rs:137` returns `Result<usize, BackupError>` and `Ok(excess)` at `:163`, but both callers discard it (`backup.rs:102`, `:128`) and no test or gate case reads it. Justification: an unused return is speculative surface in a module that otherwise only does what is asked; `Result<(), BackupError>` says the same thing. (`pub fn prune` itself is deliberate — `RESUME.md:495` documents `Backups::{new, take, prune}` as the module's surface — so only the return type is at issue, not the visibility.)
- **P3 — `ST-2`: `BackupError` does not implement `std::error::Error`,** while the module it copies does (`worktree.rs:408-414`). Nothing needs it today (`%error` needs `Display` only, and for `JoinError` too), so this is consistency with the model, not a defect.
- **P3 — `ST-3`: `Backups::new`'s doc comment restates its signature** — "The backups of the machine whose data root is `data_root`" (`backup.rs:70-72`) — which the repo's own rule calls noise, while the one *why* worth recording there is missing: every path this module hands git must be **absolute**, because `git -C <repo>` resolves a relative bundle path against the repository (the trap ADR-0029 records for worktree paths) and `fs::rename` at `backup.rs:124` would then not find it. Today `config/mod.rs:730-734` rejects a relative `data_root`/`projects_root`, so the invariant holds; the comment is where a later reader would look for it.

---

### Spec — verdict: PASS with findings

#### Acceptance, line by line

| Story line (`docs/stories/phase-2-deployable-mvp.md:306-309`) | Implemented | Proved |
| --- | --- | --- |
| `git bundle create <data>/backups/<project>-<date>.bundle --all` runs before the first session of each day | Yes — `backup.rs:92-130` (name from the UTC day, `--all`, `<data>/backups`), awaited before `provisioner().spawn` (`sessions.rs:286-287`) | Once-per-day: yes (`tests/backup.rs:59`, `:238`, and `scripts/verify.sh:1827-1843`); "before the session" is by construction only — see `SP-4` |
| The last seven bundles are kept per project; older ones deleted | Yes — `backup.rs:137-164` by name, `KEEP = 7` (`:25`) | Yes, three ways: nine real bundles (`tests/backup.rs:134`), the repair path (`:162`), and the prefix-collision case (`:193`); plus the gate's planted 2020 days (`scripts/verify.sh:1809-1839`) |
| Failure to create a bundle is logged and does not block session creation | Yes — swallowed at `sessions.rs:328-338`, one `warn` per failure | "Does not block": yes (`tests/backup.rs:264`, gate-independent). "Is logged": **not asserted anywhere** — see `SP-3` |
| Bundles are verified with `git bundle verify` and a corrupt bundle is deleted rather than kept | Yes — written aside, verified, renamed; corrupt deleted (`backup.rs:113-127`) | "Verified": yes (`tests/backup.rs:84`, `scripts/verify.sh:1845-1846` run `verify` on the kept file). "Corrupt deleted": **no test exercises that branch** — see `SP-2`; and the branch's invariant is racy — see `SP-1` |

**Also correct (with evidence).** The decision record is implemented as written: the name *is* the state (a `stat`, `backup.rs:98-104`), UTC (`:193-197`), `<name>.bundle.tmp` → verify → rename (`:113-127`), prune by name with the ten-character-day guard that keeps `holosmith` out of `holosmith2` (`:171-187`, proved by `tests/backup.rs:193`), the create takes it after the row and before the spawn (`sessions.rs:281-288`), the repository is `<projects_root>/<project>` and is not asked of the backend (`sessions.rs:312`, `worktree.rs:263`), no restore path, no timeout, no bundle on resume. The spec text matches too: `HolosmithSpec.md:56` asks exactly for this bundle and these seven, and `README.md:152` / ADR-0034 restate the same four properties. The empty-repository answer is a failure with nothing left behind, proved at `tests/backup.rs:113` (which also asserts the `.tmp` was removed).

#### Findings

- **P2 — `SP-1`: two concurrent creates on the same project can defeat "a file under the final name is always one git could read".** The temporary name is a function of project and day only (`backup.rs:113-114`), so a second create for the same project on the same day opens and **truncates the same file** the first is about to rename. Benign interleaving: both verifies fail, both log a `warn`, no bundle for the day, and the next create of the day repairs it. Hostile interleaving: A verifies, B opens and truncates the inode, A renames it into place (`backup.rs:124`) — the kept name then holds a file that was rewritten after the verification, and if B's `git bundle create` then fails (disk full, killed), the file under the final name is corrupt, which is exactly what ADR-0034 §"The bundle is verified before it is kept" and acceptance line 4 forbid. Reachable because `back_up` runs outside the create's lock scope (`sessions.rs:286`) on a multi-threaded axum runtime: two submits with different idempotency keys, or two clients, are enough. Smallest fix: make the temporary name per-attempt (`format!("{project}-{}.{pid}.bundle.tmp", ...)`) — `is_bundle_of` already refuses any name that is not exactly `<prefix><day>.bundle` (`backup.rs:171-187`), so the prune stays correct — and raise it with the ADR owner since the ADR spells `<name>.bundle.tmp`.
- **P2 — `SP-2`: acceptance line 4's "a corrupt bundle is deleted rather than kept" has no test that could fail.** The delete-on-verify-failure branch (`backup.rs:119-123`) is unreachable from the current tests: `tests/backup.rs:113` covers only the *create* refusal (a different branch, `:117-121`), and every other test and the gate case keep bundles git actually produced. Delete the `verify` call and the `remove_file` at `:121` and the whole suite stays green. Smallest fix: pull "verify then rename" into a small seam (`fn verify(&self, partial: &Path) -> Result<(), BackupError>`), unit-test it with a junk file at the `.tmp` path asserting `Err` and that the final name does not exist.
- **P3 — `SP-3`: acceptance line 3's "is logged" is asserted nowhere.** Grepping the repo for both sentences finds only `sessions.rs:332` and `:338`; `tests/backup.rs:264` asserts the `202` and the empty directory, and `case_backup`'s repository always works, so no test fails if the `warn` is deleted. Smallest fix: `grep 'are not up to date' "$serve_err"` in a case that points `projects_root` at a directory with no repository — the second boot already exists, so it is a few lines next to `case_backup`.
- **P3 — `SP-4`: acceptance line 1's ordering is proved by reading, not by a test.** The order is a fact in the code (`sessions.rs:286` awaits `back_up`, `:287` then spawns), and ADR-0034 gives the right reason, but swapping those two lines leaves everything green — the stub backend makes no branch the bundle could be shown to lack, so nothing observes that the bundle predates the session. Recorded so the next reader does not read the green gate as proof of ordering.

---

### Verified by reading

| Claim | Evidence |
| --- | --- |
| `project_backup` key exists, defaults off, is the only place it exists | `config/schema.rs:158-162`, `config/mod.rs:197`, `:234`, golden `tests/golden/config-default.toml:16` |
| Bundle path/name are `<data>/backups/<project>-<YYYY-MM-DD>.bundle`, UTC, `--all` | `backup.rs:69-73`, `:92`, `:114`, `:193-197` |
| Once per day is a `stat`, and the second call still prunes | `backup.rs:98-104`; `tests/backup.rs:59`, `:162` |
| Verify-then-rename, corrupt deleted, `.tmp` removed on every failure | `backup.rs:113-127` |
| Seven kept per project by name; prefix collision safe | `backup.rs:137-187`; `tests/backup.rs:134`, `:193` |
| Calendar arithmetic is right for the epoch, a leap day, and the test's day | `backup.rs:262-271`; constant `1_793_750_400 = 20_761 × 86_400` |
| Create takes it after the row, before the provisioner, with failures swallowed | `sessions.rs:281-288`, `:307-340` |
| Subprocess off the runtime worker; no DB guard across that `await` | `sessions.rs:318-319`; `Slot` carries owned values (`:385-392`) |
| No logging inside the std-only module; error is the operator's sentence | no `tracing` in `backup.rs`; `backup.rs:43-53`; `sessions.rs:328-338` |
| No SQL, no new crate, no `Cargo.toml`, `pins/` untouched | diff artifact — no such hunks |
| Gate: one `case_*`, own boot, own config/data, in `serve_cases` | `scripts/verify.sh:324`, `:1791-1816`; `docs/build.md:205-206` |
| Docs deliverables: ADR count, bullet in the list, build.md, glossary | `docs/stories/README.md:116`, `:152`; `docs/build.md:181-185`, `:206`; `GLOSSARY.md:381-383` |
| Test helpers the new tests call actually exist with those signatures | `tests/support/mod.rs:50`, `:118`, `:124`, `:131`, `:174`, `:245`, `:481`, `:499`; `http/app.rs:400`; `runtime.rs:25-27` |

### Could not verify without running

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` (including the new `crates/holosmith/tests/backup.rs`) — nothing asserts the tree compiles. One specific thing I believe compiles but cannot prove: `git(repository, &["bundle", "create", &path(&partial), "--all"])` (`backup.rs:114`) relies on `&String` → `&str` deref coercion at an array-literal element; same for `&entry.file_name().to_string_lossy()` at `:150`.
- `scripts/verify.sh` (or `scripts/verify.sh --no-build` against an existing `dist/`) — needed to run `case_backup`: whether the plant/prune/verify/heads assertions pass, whether the two creates return `202` on this machine, and whether `git bundle verify` accepts the kept file.
- Real-run behaviour of the two socket tests (`tests/backup.rs:238`, `:264`) under the shared harness, and whether `case_backup` is stable in a machine with an old `git` (<2.38 is refused by the preflight, but `bundle` subcommand behaviour across versions is untested here).
- The `SP-1` race: I argue it from the code (`backup.rs:113-124`) and could not schedule it; a stress test would be needed to show the hostile interleaving.

**Summary.** Standards: 3 findings (worst P3 — unused `prune` return / missing `Error` impl / a restating doc comment). Spec: 4 findings (worst P2 — the shared `.tmp` name lets two same-project creates defeat "verified before kept"; and the corrupt-bundle delete branch has no test that could fail). No P0/P1 on either axis; merge verdict: **OK with notes**.
## Resolution

Every finding was real and every one is fixed. The two P2s were the valuable ones: a race that could put a
rewritten file under the name that verified, and a delete branch that no test could reach.

| Finding | Triage | How / why |
| --- | --- | --- |
| **SP-1 P2** — the temporary name was a function of project and day only, so two creates on one project in flight at once could have the second `git bundle create` truncate the file the first was about to rename; if the second then failed, the name that verified would hold a file rewritten **after** the check | **Fixed** | The temporary name is now per attempt — `<name>.bundle.<pid>.<n>.tmp`, from one `AtomicU64` — and `is_bundle_of` already accepts only exactly `<prefix><day>.bundle`, so the prune is unaffected. ADR-0034 gained a paragraph saying why, because the ADR had spelled the shared name. Reachable in production: `back_up` runs outside the create's lock scope. |
| **SP-2 P2** — acceptance line 4's "a corrupt bundle is deleted rather than kept" had **no test that could fail**: git always writes a bundle it can verify, so the delete branch was unreachable and deleting the `remove_file` left the suite green | **Fixed, and proved by mutation** | The verify-and-rename step is now `Backups::keep`, and `a_bundle_git_cannot_read_is_deleted_rather_than_kept` hands it a junk file: `Err`, no `.tmp`, no final name. **Mutation run:** with the `remove_file` deleted the test fails. |
| **SP-3 P3** — acceptance line 3's "is logged" was asserted nowhere: `tests/backup.rs` proved the 202 and the empty directory, but nothing failed if the `warn` was deleted | **Fixed** | `case_backup` now creates a session for a project with no repository at all (a third, hence `max_live_sessions = 3`), asserts the `202`, and `grep`s the boot's stderr for "the project's backups are not up to date". Two new assertions, and the gate went 325 → 328. |
| **SP-4 P3** — acceptance line 1's *ordering* ("before the first session") is proved by reading, not by a test: swapping the `back_up` call and the `provisioner().spawn` leaves everything green | **Dismissed, with the reason recorded** | Nothing observable can distinguish the two orders on a stub backend — the bundle is of the host checkout and the stub creates no branch — and the ordering is the code's literal shape (`back_up(…).await;` then `spawn(session)`). A test would have to invent an observation point, which is a bigger change than the story. Recorded here and in ADR-0034's "What the gate shows" so the green gate is not read as proof of ordering. |
| **ST-1 P3** — `prune` answered `Result<usize, …>` and both callers threw the count away | **Fixed** | `Result<(), BackupError>`. Speculative surface in a module that otherwise only does what is asked. |
| **ST-2 P3** — `BackupError` did not implement `std::error::Error`, while `worktree::GitError` (the module this one copies) does | **Fixed** | An empty impl with a comment saying why it is empty (neither variant wraps another error). |
| **ST-3 P3** — `Backups::new`'s doc restated its signature, and the *why* worth recording was missing: every path handed to git must be absolute, because `git -C <repository>` resolves a relative bundle path against the repository | **Fixed** | The doc now states the invariant, cites ADR-0029's measured trap, and names the config layer's absolute-path rule as what makes it hold. |

The review's "could not verify without running" list was run afterwards, in order: `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test` (244 lib; 9 `tests/backup.rs`), `cd web && npm
test` (34), `cd web && npx tsc --noEmit`, and `scripts/verify.sh` (46 cases + the CORS sweep, 328
assertions, SIGTERM exit 0). Its `SP-1` race was not scheduled — a stress test would be needed — which is
why the fix is structural (unique name per attempt) rather than a lock.
