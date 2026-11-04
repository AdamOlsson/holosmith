# Review — PER-56 (HS-024, Docker preflight)

- **Issue:** PER-56, story HS-024 — "Backend selection and Docker preflight", from
  `docs/stories/phase-1b-docker-backend.md`.
- **Fixed point:** `fbc0659` (the commit before this story's work). Diff artifact:
  `/tmp/holosmith/reviews/PER-56-diff.txt` — 16 files at the time of the review (1276 lines), regenerated
  after the fixes (1489 lines).
- **Spec sources:** `HolosmithSpec.md` item 10 §1 and §5/§6, item 2, V2, and the item 10 API table row for
  `holosmith doctor`; the HS-024 slice; the Linear body; the plan in `/tmp/holosmith/plans/PER-56-linear.md`;
  ADR-0028 (this change's own) plus ADR-0006 and ADR-0015.
- **Standards sources:** `AGENTS.md`, `GLOSSARY.md`, `docs/build.md`, `docs/stories/README.md`, the ADRs, and
  `RESUME.md`'s landmine list.
- **Verdicts:** Spec — **BLOCK**, one P1 and six P2/P3. Standards — OK with notes, two P2 and five P3.

The review below is the reviewer's, verbatim; every finding is answered in the Resolution at the end.

---

## Spec

**S1 · P1 — the preflight runs *after* the database is opened and migrated, and the code comment and ADR claim the opposite.**

`crates/holosmith/src/commands/serve.rs` — `run()`, added hunk at `:91-111` vs. existing `:69`.

```
:69      let database = match Database::open(          // opens, creates and migrates the file
:86-89   if let Some(backend) = args.backend { ... }    // backend override applied here
:91-93   // ... and before the database is opened because a machine that cannot run sessions
         //     should not migrate one.
:98      if backend == Backend::Docker {               // preflight
```

`Database::open` is documented as "Open (creating it if needed) and migrate the database at `path`" (`crates/holosmith/src/db.rs:60-71`), and `serve.rs:59`'s `create_dir_all` already created the data root. So a Docker run that fails the preflight has, by then, created `<data_root>/holosmith.db` and applied migrations. `docs/adr/0028-the-docker-preflight.md:11-13` states the delivered promise as "before the database is opened and before the listener binds; a failure prints every complaint and exits 1. **No banner, no port, no migration**." The banner and the port hold; the migration does not. Nothing catches it: `scripts/verify.sh:1475-1501` asserts exit 1, no banner line and the sentence, and never looks at `$WORK/data-docker`.

Smallest fix: move the two-party block (the `args.backend` override at `:86-89` and the preflight at `:91-111`) above `let database = match Database::open(` at `:69`. It stays after `create_dir_all` (`:59`), which the `df` check needs, and the data-root failure still reports first (`tests/serve_cli.rs:500`).

**S2 · P2 — a `hub` run, which runs no sessions, now refuses to start on a machine with the default backend and no daemon.**

`crates/holosmith/src/commands/serve.rs:98` gates on the backend alone; `crates/holosmith/src/runtime.rs:17-29` makes `Docker` the default; `crates/holosmith/src/config/mod.rs:179-183`. The spec exempts hubs explicitly: `HolosmithSpec.md:371` — "`hub deploy` runs steps 1 (**without the Docker and git checks**), 2 (without the docker group), 5, 6 and 7" — and `GLOSSARY.md:30` — a hub "Runs no sessions." So `role = "hub"` with the shipped default `backend = "docker"` is now refused unless the hub has a Docker daemon and git ≥ 2.38, which it does not need. ADR-0028 never mentions roles. Smallest fix: `if backend == Backend::Docker && loaded.config.role != Role::Hub`, or state in `docs/build.md:269-273` that a hub sets `backend = "stub"`.

**S3 · P2 — the unix-socket sentence names the wrong fix for the commonest real failure, contradicting `docs/build.md` in the same diff.**

`crates/holosmith/src/preflight.rs:172-176`: every `ping_unix` error becomes

```
"the docker daemon at {host} did not answer ({reason}): start it with
 `sudo systemctl start docker`, or set docker_host to the daemon you run"
```

A socket the operator may not open yields `reason = "Permission denied (os error 13)"` and the advice "start it with `sudo systemctl start docker`", which does not fix a group problem — and `docs/build.md:280-281` promises "Put yourself in the `docker` group or the socket answers `permission denied` — **the preflight's sentence names it**". It does not. Smallest fix: branch on `error.kind() == ErrorKind::PermissionDenied` in `ping_unix` and say "add yourself to the `docker` group (then log back in)" for that case only.

**S4 · P2 — every `df` failure is blamed on coreutils, including the ones where `df` ran fine and the path is the problem.**

`crates/holosmith/src/preflight.rs:293-313`: the non-zero-exit arm (`:296-300`) and the spawn-failure arm (`:303-306`) both end "install coreutils", and `output.stderr` is discarded. `df -Pk /data` on a data root under a directory the operator cannot traverse, or one that no longer exists, exits non-zero and reports "install coreutils" — a fix that changes nothing. Smallest fix: in the non-zero-exit arm include `String::from_utf8_lossy(&output.stderr)`, and say "check that the data root exists and is readable"; keep "install coreutils" for the spawn-failure arm.

**S5 · P2 — four places plus a pinned unit test say HS-046 owns ssh daemons; HS-046 is the phase-1 verification run, and no story owns ssh.**

`crates/holosmith/src/preflight.rs:14` ("HS-046 owns ssh hosts"), `:187` ("check it by hand until HS-046"), `docs/build.md:280`, `docs/adr/0028-the-docker-preflight.md:37`; and `crates/holosmith/src/preflight.rs:394-399` asserts `complaint.sentence.contains("HS-046")`, so the wrong number is now a test. `docs/stories/phase-1b-docker-backend.md:368` — "HS-046 — Phase-1 verification run"; HS-045's acceptance (`:360`) lists no ssh check, and HS-073 is `--ssh` *deploys*. The operator is promised a fix that no slice of work delivers. Smallest fix: replace with "which this build cannot check; run the daemon on this machine" and drop the story number (and the assertion on it).

**S6 · P2 — three documents disagree on whether a stub run is git-checked, and the code is a fourth opinion.**

`crates/holosmith/src/preflight.rs:138-144` — "Git is checked for both, because it is the session's own tool … and this is the only reason `check` takes the backend too", but `pub fn check(config: &Config)` (`:145`) takes no backend. `docs/adr/0028-the-docker-preflight.md:22` — "`stub` runs are **not checked beyond git**". `docs/build.md:272` — "`--backend stub` **skips all of it**". The code (`serve.rs:98`) checks nothing for a stub run, so `build.md` is right and two of the three texts are stale. Smallest fix: delete the last clause at `preflight.rs:142-144` and change ADR-0028:22 to "`stub` runs are not checked at all".

**S7 · P3 — the dropped plan item's doc comment survived on `Complaint`.**

`crates/holosmith/src/preflight.rs:77-79`: "`DockerBackend` carries it, so a session that reaches the backend anyway lands in `error` with the same words (HS-024) … Three readers, one sentence." There is no such field (`backend/docker.rs:24-26`, `pub struct DockerBackend;`) and `backend/docker.rs:9-11` says the opposite in this same diff ("A backend that reported its own preflight would be a second opinion about the same machine"). Plan item 5 records the drop; the comment is the leftover. Fix: "two readers — `serve` and HS-045's `doctor`".

**Spec verdict: BLOCK** — S1 is a documented contract (`ADR-0028:11-13`, `serve.rs:91-93`) that the code does not implement, in the same diff, with a two-line fix. S2–S6 are real spec-bullet misses ("actionable single sentences naming the fix", the hub role) that should be fixed before release.

## Standards

**T1 · P2 — Duplicated Code (baseline, judgement): the fake-daemon fixture is pasted twice in one file.**

`crates/holosmith/tests/serve_cli.rs:279-287` and `:324-332` are the same nine lines — `UnixListener::bind`, `thread::spawn`, `accept`, `read` 512 bytes, `write_all(b"HTTP/1.1 200 OK\r\n\r\nOK")`. `crates/holosmith/tests/preflight.rs:88-107` is a third, richer copy (`fake_daemon`). Two copies of the identical fixture in one file is exactly the shape the baseline calls out. Smallest fix: `fn docker_ping_socket(dir: &TempDir) -> PathBuf` beside `config()` in `serve_cli.rs`, called twice.

**T2 · P2 — two tests now depend on the host's real `git` version and real free space, which the preflight tests deliberately avoid.**

`crates/holosmith/tests/serve_cli.rs:292` and `:337` set `min_free_gb = 1`, and both tests then run the production `df` on the temp filesystem and the production `git --version` against `PATH`. A CI box or container with < 1 GiB free on `/tmp` fails `a_flag_backend_wins_over_the_config_file` and `the_default_backend_is_docker_and_the_log_line_says_so` for a reason that has nothing to do with the assertion. `tests/preflight.rs:6-11` states the rule the other way ("keeps the tests independent of this machine's git version and free space — which is exactly what a test against the real `df` would make flaky"), so the file that follows it and the file that breaks it are both in this diff. Smallest fix: `min_free_gb = 0` (allowed — `config/mod.rs:202` has no floor and `:317-318` truncates, so `0 >= 0` passes); the `git` dependency cannot be removed without a seam in `serve` and is worth a comment instead.

**T3 · P3 — `conformance()` is a shared assertion helper, not a suite that issues the calls, and `session()` is now a third copy.**

`crates/holosmith/tests/backend_contract.rs:50-75` takes `refused: Result<(), BackendError>` already computed by the caller (`:81`, `:89`), so case 2 asserts a message, not that the suite's own call failed — a backend whose only refusal is the one the caller happened to pick passes. The undo loop and the naming assertion are genuinely shared and worth keeping. `:24-40`'s `session()` is character-for-character `backend/docker.rs:106-124` and `stub.rs:441-459`. Judgement call, report-only.

**T4 · P3 — two counts in `RESUME.md` are now stale and the diff moved one of them further.**

`RESUME.md:709` "The current gate is 36 cases / 169 assertions" — this diff adds `case_docker_preflight` (`scripts/verify.sh:309`), so 37. `RESUME.md:711` "the count (now **twenty-six**)" while `docs/stories/README.md:117` now says twenty-eight (and said twenty-seven before). The ADR count drift predates this diff; the case count is new.

**T5 · P3 — GLOSSARY lists `Doctor` as a word to avoid, for a concept the spec gives a command named `doctor`.**

`GLOSSARY.md:19` — `_Avoid_: Doctor, sanity check, health check, smoke test`. HS-045 (`docs/stories/phase-1b-docker-backend.md:352`) ships `holosmith doctor`, and `HolosmithSpec.md:348` already names it in the API table. A later reader looking up "doctor" is told not to use the word. Smallest fix: drop `Doctor,` from that line.

**T6 · P3 — misattributed ownership in the module doc.**

`crates/holosmith/src/preflight.rs:10` — "the systemd and Tailscale questions HS-045 and HS-044 own". HS-044 is "Startup reconcile and crash safety (V9)" (`docs/stories/phase-1b-docker-backend.md:336-350`); the systemd, Tailscale and port checks are HS-045's alone (`:360`).

**T7 · P3 — `ping`'s contract is looser than its doc, and two of the three external calls can block without bound.**

`crates/holosmith/src/preflight.rs:226-249`: the doc says "the answer's status line and body" but only `answer.lines().next()` is inspected; the body `OK` that ADR-0028:47 leans on is never checked. `read_to_string` over `take(1024)` (`:237-238`) needs EOF, so a peer that answers `200 OK` and honours keep-alive instead of `Connection: close` is reported as "did not answer" after the 3 s read timeout. On the "can it hang a startup" question: reads and TCP connects are bounded (`:201-207`, `:218`), `UnixStream::connect` (`:199`) is not (std offers no timeout), and — worse — `git --version` (`:255`) and `df -Pk` (`:294`) are `Command::output()` with no timeout at all, so a `df` on a hung NFS/CIFS data root blocks `serve`'s startup for ever instead of failing it. Worth a recorded limitation in ADR-0028 at minimum.

**Standards verdict: OK with notes** — no documented-standard breach: `case_docker_preflight` exists (`RESUME.md:708`'s rule), no `awk`/`shellcheck` (`RESUME.md:205`), no new dependency, no new config key, no schema or default-backend change, the banner's eight lines are intact (`tests/serve_cli.rs:180-195` runs with `--backend stub`, and the preflight's only new line is `tracing::debug!`, suppressed at the default `info`), and the fake's claim is proved rather than asserted (`tests/preflight.rs:186-205` fails a `500` answer, so a connect-only check cannot pass). T1–T2 should be fixed before release; the rest are notes.

## Verification

Verified by reading:

- `serve`'s order: `logging::init` → config load/override problems → `create_dir_all` (`serve.rs:59`) → `Database::open` (`:69`) → `--backend` override (`:86`) → preflight (`:98`) → bind (`:35` of the async body) → banner. S1 is a direct reading of two line numbers.
- `docker_host`'s validator accepts `unix://`, `tcp://` and `ssh://` (`config/mod.rs:504-515`), so the ssh branch is reachable and `build.md:279`'s claim about the validator is true.
- The `_ping` check: status-line only (`preflight.rs:240-249`); a listener that accepts and says nothing becomes a complaint via the 3 s read timeout (`:201-207`), not a pass; IPv4-first `to_socket_addrs` for `tcp://`.
- `parse_df_available` (`:333-336`): `-P` gives one unwrapped line per filesystem, fourth field of line 2, so a long device name and a long mount path are both covered; a wrapped answer returns `None` and becomes a complaint (`:310-314`), and the unit test at `:355-370` pins that.
- `Complaint`'s sentence is asserted by its parts, not its prose, in both `tests/preflight.rs` and `scripts/verify.sh:1489-1499`.
- `Step::ALL` is four steps (`backend/mod.rs:48`); the stub's failure text contains the step word (`stub.rs:255-257`), so `conformance(..., "container")` and `(..., "network")` both hold.
- `case_docker_preflight` is deterministic on a machine *with* a daemon (the socket path cannot exist), needs no `start_serve`, leaves only `$WORK/data-docker`, and is registered at `verify.sh:309`.
- `docs/stories/README.md:117` says twenty-eight and links `../adr/0028-the-docker-preflight.md`, which exists; `docs/build.md:267-290`'s Desktop caveat is in the prerequisites doc the story names and its `docker_host`/git paragraphs match the config validator and `parse_git_version`.

Not verified — must be run (I ran nothing):

1. `cargo test -p holosmith` — `preflight` (6 integration + 6 unit), `backend_contract` (2), `serve_cli` (all, including the two daemon-fixture tests that need host `git` ≥ 2.38 and ≥ 1 GiB free — the T2 risk).
2. `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` (new `let _ = request;`, `take(1024)`, the long `tracing::debug!` argument list at `serve.rs:106-111`).
3. `scripts/verify.sh` — confirms the gate is now 37 cases and that `case_docker_preflight` passes from the shipped artifact; also confirms nothing else in `serve_cases` broke from the shared-config change.
4. S1 confirmation: on a machine with no daemon, `holosmith serve --backend docker --data-dir /tmp/x` then `ls /tmp/x` — I expect `holosmith.db` present, which is the defect. The gate case does not assert this either way.
5. S3 confirmation: `docker_host = "unix:///var/run/docker.sock"` as a user not in the `docker` group, and read the sentence.
6. S4 confirmation: `df -Pk` on a data root under a directory with mode 0700 owned by another user.
7. Mutation for the ping claim: invert `if !status.contains(" 200")` to always pass and confirm `a_daemon_that_answers_something_else_is_a_complaint` (`tests/preflight.rs:186-205`) fails; likewise delete the `_ =>` arm of `docker()` and confirm `a_docker_host_that_is_not_a_scheme_is_refused_with_the_two_spellings` fails.
---

## Resolution

Every finding is answered: `fixed` names the change, `dismissed` names the document it rests on.

### Spec axis

| Finding | Severity | Resolution |
| --- | --- | --- |
| **S1** — the preflight ran *after* `Database::open`, so a refused Docker startup had already created and migrated `app.db`, while `serve.rs`'s comment and ADR-0028 claimed "no migration" | **P1** | **Fixed.** The `--backend` override and the preflight move above `Database::open` (and stay after `create_dir_all`, which the disk check needs). This was this session's own ordering bug: my scripted move in the first draft put the block in the wrong place and I did not re-read the order. `case_docker_preflight` now also asserts that no `app.db` appeared, so the claim cannot rot again. |
| **S2** — a `hub`, which runs no sessions, was refused on a machine with the default backend and no daemon | P2 | **Fixed.** The gate is `backend == Docker && role != Hub`, and `tests/serve_cli.rs` gains `a_hub_is_not_checked_for_docker_because_it_runs_no_sessions`, which boots a hub with `docker_host` at a socket that cannot exist and `min_free_gb` at its default 20 on a machine with 3 GB free. ADR-0028 and `docs/build.md` both say why, quoting item 10 §6. |
| **S3** — `permission denied` on the socket got the stopped-daemon advice, contradicting `docs/build.md`'s promise in the same diff | P2 | **Fixed.** `ping` reports an `Unreachable { message, kind }`, and `socket_sentence` branches on `ErrorKind::PermissionDenied` to name `usermod -aG docker` and not `systemctl`. The sentence is a function so each branch has a unit test — the permission one cannot be arranged against a real socket by a test process. |
| **S4** — every `df` failure was blamed on coreutils, including a data root it could not traverse | P2 | **Fixed.** The non-zero-exit arm puts `df`'s own stderr in the sentence and says "check that the data root exists and that this user can reach it"; "install coreutils" is kept only for the spawn failure, which is the case it describes. |
| **S5** — four places and a pinned test said HS-046 owns ssh daemons; HS-046 is the phase-1 verification run | P2 | **Fixed.** The story number is gone from the module doc, the sentence, ADR-0028 and `docs/build.md`; the doc now says no story owns an ssh check yet and that one, when it arrives, replaces the complaint. The test asserts "cannot check" rather than a story number. |
| **S6** — three documents disagreed about whether a stub run is git-checked | P2 | **Fixed.** All three now say a stub run is not checked at all, which is what the code does (`check` is called only for a Docker, non-hub run). The stale clause that also claimed `check` took the backend is gone. |
| **S7** — the dropped plan item's doc comment survived on `Complaint` | P3 | **Fixed** — the comment names two readers (`serve` and `doctor`) and says why the backend does not carry the sentence. |

### Standards axis

| Finding | Severity | Resolution |
| --- | --- | --- |
| **T1** — the fake-daemon fixture was pasted twice in `serve_cli.rs` | P2 | **Fixed.** One `docker_ping_socket(dir) -> PathBuf` helper beside `config()`, called twice. |
| **T2** — two `serve_cli.rs` tests depended on the host's real `df` free space | P2 | **Fixed.** `min_free_gb = 0` in both (the validator has no floor, and `0 >= 0` passes), with a comment saying the git check still runs against the host's own git — which is what a Docker run on this machine would use, so it is a fact rather than a flake. |
| **T3** — `conformance()` takes the refusal the caller computed, so it asserts a message rather than issuing the call; and `session()` is a third copy | P3 | **Dismissed, with the reason written into the file.** A suite that issues the call itself needs an `async` closure generic over a borrow of the backend, which stable Rust cannot express without a boxed future per case; the shared assertion plus the caller's call is the honest seed, and the file says so. `session()` is a fixture in a `#[cfg(test)]` module of two other files, which an integration test cannot import — a shared copy would mean exporting a test-only constructor through the lib for three tests. |
| **T4** — `RESUME.md`'s counts were stale | P3 | **Fixed** in this session's handoff refresh (cases, assertions, ADR count, next issue). |
| **T5** — `GLOSSARY.md` listed `Doctor` as a word to avoid, for a command the spec names | P3 | **Fixed** — the **Preflight** entry's avoid-list explains that `doctor` is the command and a preflight is not one. |
| **T6** — the module doc attributed the systemd/Tailscale checks to HS-045 *and* HS-044 | P3 | **Fixed** — HS-045 alone; HS-044 is the startup reconcile, not a port check. |
| **T7** — `ping`'s doc promised a body it did not check; `read_to_string` needed EOF; the two commands have no timeout | P3 | **Fixed for the first two, recorded for the third.** `ping` now does one read, checks the status line always and the body when it arrived (Docker's own `OK` contract), and the doc explains why one read rather than a read-to-EOF. The commands' lack of a timeout is now an explicit paragraph in ADR-0028 saying what it costs (a hung network filesystem blocks startup) and when to revisit it. |

### Found while fixing, not in the review

- **The gate itself broke, and that is what the first `verify.sh` run said.** `scripts/check-static.sh` boots the
  artifact in an empty root to prove it needs no loader — and that root has no `git`, no `df` and no daemon, so
  the new preflight refused there. The empty-root run now passes `--backend stub` (it is about linking, not
  about a machine's backend choice), with the reason in the script.
- **A long line in the ADR-0028 additions** and a duplicated `Database::open` block from my scripted move are
  gone; the second is what S1 caught.

### A note on this file's formatting

The two `MD040` lint warnings on this file are in the reviewer's verbatim body (fenced blocks without a
language). The body is not edited, by the rule that a saved review is verbatim.
