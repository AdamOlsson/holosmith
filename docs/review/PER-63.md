# Review — PER-63 / HS-031 (attached `docker run`, pi RPC, ready handshake, project trust)

Fixed point: `dba7faf` (the last commit before the story). Diff: `dba7faf...HEAD`, at review time
`/tmp/holosmith/reviews/PER-63-diff.txt` (3,660 lines). Reviewer: a fresh `reviewer` child, read-only, running
the code-review skill with the Linear issue + the story slice + the spec items as the Spec axis and
`AGENTS.md`/the ADRs/`docs/build.md` as the Standards axis. Saved verbatim below, then triaged in
`## Resolution`. The output beginning ("I have read the brief…") is the child's preamble.

I have read the brief, the full diff artifact, and every file the diff names in the committed tree. Here is the review.

---

# Review — PER-63 / HS-031 (attached `docker run`, pi RPC, ready handshake, project trust)

Scope reviewed: `/tmp/holosmith/reviews/PER-63-diff.txt` (`dba7faf...HEAD`), plus the committed files it touches and the contracts it claims (`HolosmithSpec.md` items 2/6/15 + D1 + risk table, `GLOSSARY.md`, ADR-0019/0035/0036/0037/0038/0039, `docs/build.md`, `scripts/verify.sh`, `crates/holosmith/src/{agent,exit,provisioner,deleter,backend/docker,backend/mod,backend/stub,container,docker}.rs`, `tests/{agent,container,provisioning,exit}.rs`, `sessions/state.rs`, `http/sessions.rs`). Read-only: nothing edited, nothing run.

## Correct (evidence for what already works)

- The ready handshake is a real protocol fact, not a sleep: `agent.rs:376-396` (`ready()` → `get_state` with `id: "ready-1"`), routed by id in `read_records` (`agent.rs:416-520`), bounded by `READY_TIMEOUT` (30 s), and `success: false` is carried as the agent's own words (`AgentError::Refused`). `tests/agent.rs:...` and the gate's `idle` wait both depend on it, so `starting → idle` can no longer be reached without pi answering.
- Framing is LF-only on raw bytes (`read_until(b'\n')`, one optional `\r` stripped, `agent.rs:416-450`) — matches ADR-0039's claim and avoids the `U+2028` trap.
- Both pipes are drained from launch, the exit is sent only after the stdout reader ends (`agent.rs:299-330`), and `kill_on_drop(true)` is set — no pipe-nobody-drains hazard, and the exit cannot overtake the last `response`.
- The generation check does what ADR-0039 says: `docker.rs:641-690` compares the registry's generation *at the moment the exit arrives*, and the registry entry is replaced and never removed (`docker.rs:71-80`). The `deleting` row cannot be turned into `error` anyway (`state.rs:225-229` allows `UnexpectedExit` only from `starting|idle|running`).
- The seed is written once and never overwrites pi's own files (`agent.rs:105-126`, test at `agent.rs:...`), the template carries no key (`agent.rs:...`), and the launch line is genuinely allow-listed (`agent.rs:180-200`; the unit test even asserts the line contains no `=`).
- `ContainerSettings::at` now takes both roots, so no test creates `/var/lib/holosmith` by accident; every call site in the tree is updated (`backend/mod.rs:502,509`, `backend/docker.rs:713-716`, `tests/backend_contract.rs:144`, `tests/worktree.rs:79`, `tests/provisioning.rs:393`), and `run_container`/`IDLE_COMMAND` have no remaining referents outside `RESUME.md` (scratch) and the historical ADRs.

---

## Axis 1 — Standards: **PASS WITH FINDINGS**

### S-1 · P2 · `crates/holosmith/src/backend/docker.rs:1-22` (module doc, untouched hunk) — the file's "why" doc still says the pi step refuses
```
//! provisioned. **Three of the four provisioning steps are real:** the worktree (HS-025), the session's
//! private network (HS-028) and the container itself (HS-028). The **pi** step still refuses, and so does
//! every lifecycle operation, with [`BackendError::Unsupported`] or [`BackendError::unsupported_step`],
```
and at `:21-22`, "HS-031 replaces the detached launch with pi's RPC over the container's stdio." This is the file whose `provision` arm the story rewrote (`docker.rs:507-517` → `Step::Pi => self.provision_agent(session).await`). The diff edited `provision`'s doc and left the module doc — the repo's stated convention ("module docs that explain *why*") now lies in the first ten lines of the story's own file.
**Proof:** read `docker.rs:1-22` against `docker.rs:507-517`; `grep -n "still refuses" crates/holosmith/src` returns this file and nothing else.

### S-2 · P2 · `crates/holosmith/src/provisioner.rs:199-205` — the constraint the story closes is documented as still open
```
    /// **The ordering it depends on, and which HS-031 has to keep:** … nothing
    /// in this build observes an exit, which is why no test here can reach it. ADR-0038 records it as
    /// HS-031's constraint (await the supervisor's report between the rungs and the resume, or key the flag
    /// to the container's incarnation).
```
Every clause is now false: this story *is* HS-031, it *does* observe an exit, ADR-0039 says the constraint is **closed** by the second option (`docs/adr/0039…:120-135`), and ADR-0039's own mutation list says `tests/exit.rs`'s provision-clear test (`tests/exit.rs:290`) does reach the ordering. A reader landing here concludes the design question is still deferred.
**Proof:** diff `clear_expected_stop`'s doc against ADR-0039's "The exit is the server's to record" section; the word "incarnation" appears in the ADR as the *chosen* mechanism, not an open option.

### S-3 · P2 · `crates/holosmith/src/agent.rs:96-97` — a rationale pointer to a sentence that does not exist
```
/// `--approve` is open question 9's answer, and [`crate::backend::docker`]'s module doc says why the
/// alternative (`defaultProjectTrust: "always"` in the seed) is not this build's choice.
```
**Proof:** `grep -rn defaultProjectTrust` in the tree returns exactly two hits — ADR-0039:78 and this comment. The Docker backend's module doc contains no trust discussion (see S-1 for what it does contain).
**Smallest fix:** point at `docs/adr/0039`'s "Project trust is `--approve`" section, or move the two sentences here.

### S-4 · P2 · `crates/holosmith/src/exit.rs:49-52` + `GLOSSARY.md` "Session log" — "the whole of stderr" is no longer true
```
/// Five, because the row is read on a phone … The whole of stderr goes
/// to [`session.log`](Exits) regardless.
```
`agent.rs:80-88` now keeps only `STDERR_TAIL = 64 * 1024` bytes, and `Exits::record` appends that tail only (`exit.rs:170`). ADR-0039's "Two readers, because both pipes block the process" argues *for* the bound — that part is settled — but it does not own these two sentences, and the glossary repeats the stronger claim ("verbatim").
**Proof:** `STDERR_TAIL` vs the two doc sentences; the `read_stderr` drain (`agent.rs:...`) drops from the front (`kept.drain(..overflow)`).

### S-5 · P2 · `crates/holosmith/src/deleter.rs:80-85` and ADR-0039 — the reason given for the delete's flag write is impossible
```
        // The container is about to be removed by the server, so its exit is not news. `Stopper` owns the
        // flag for a *stop* (HS-030); a delete is the same fact by another route, and since HS-031 there is
        // an observer that would otherwise classify the removal as a crash and move a `deleting` row to
        // `error`.
```
ADR-0039 repeats it ("because the observer would otherwise classify the server's own `docker rm -f` as a crash and move a `deleting` row to `error`"). The table has no such row: `state.rs:225-229`
```
Transition { from: FromStates::OneOf(&[Status::Starting, Status::Idle, Status::Running]),
             trigger: Trigger::UnexpectedExit, to: Status::Error }
```
and `state.rs:620-626` asserts that for a `deleting` row only `Delete` is legal. The DELETE handler writes `deleting` before the deleter task exists (`http/sessions.rs:987-998`), so the observer could at worst log a refused transition.
**Verdict on the code:** the flag write is harmless and arguably still right (it keeps the log quiet and the record honest); the *stated* justification is wrong in both places, which is what a later story (HS-044) would build on. **Proof:** the two `state.rs` sites above; a mutation deleting `deleter.rs:85` leaves `cargo test` green (which is the tell that it is not load-bearing for the reason claimed).

### S-6 · P3 · two dead public accessors added in one diff — `crates/holosmith/src/exit.rs:155-157` and `:226-229`
```
    /// The log this classifier writes into, for a caller that wants to append the launch line.
    pub fn session_log(&self) -> SessionLog { … }
    /// `<data>/sessions/<id>/session.log`, for a caller that reads what was recorded.
    pub fn log_path(&self, session_id: &str) -> PathBuf { … }
```
Nothing calls either: the launch line is appended by the *backend's* own `SessionLog` (`docker.rs:96`, `docker.rs:498-501`), and the test that reads the log rebuilds the path by hand (`tests/agent.rs`: `data/sessions/<ID>/…join(LOG_FILE)`). The first doc names a caller that does not exist. Note this also means the story added a second derivation of one path (`Exits::for_database` → db parent, `DockerBackend::log` → `settings.data_root`) while claiming "exactly one rule for where a session's files are" (`exit.rs:139-142`).

### S-7 · P3 · `crates/holosmith/src/agent.rs:332-372` — `request`'s write has no bound, so "a process that never answers fails the launch" is only half true
```
        let mut stdin = self.stdin.lock().await;
        if let Err(error) = stdin.write_all(&line).await { … }
        …
        match tokio::time::timeout(timeout, receiver).await {
```
The timeout wraps the reply only. A pi that stops reading its stdin blocks `write_all` forever while holding the async mutex; the pi step then never returns, the row stays `starting`, and a second command from HS-032 waits behind the same lock with its own deadline not yet started. The module doc at `agent.rs:17-20` promises the opposite for the handshake (small payload, so unreachable today) — but this is the seam HS-032/HS-033 will widen.
**Proof (cheap, no Docker):** a fake agent of `sh -c 'sleep 60'` that never reads; `request` with a command larger than the 64 KiB pipe buffer and a 50 ms bound — it hangs, and `READY_TIMEOUT` never fires.

### S-8 · P3 · vocabulary: `GLOSSARY.md:259` bolds a term that has no entry, and `Exit` now names two ideas
`**launch generation**` is used as a cross-reference in the new "Agent process" entry, but the glossary has no such entry (every other bolded term in the file is an entry — `**mount**`, `**exit classifier**`, `**step**`). The brief names `generation` as a vocabulary item, and the field is `Launched::generation` (`docker.rs:84`). Separately, `agent::Exit { code, stderr }` (`agent.rs:203-212`) and `exit::Exit { Expected | Clean | Failed }` (`exit.rs:57-72`) are two types with one name and different meanings, while the glossary says the concept's machine name is `ProcessEnd`.
**Smallest fix:** add a two-line **Launch generation** entry and one clause saying `agent::Exit` is the raw report and `exit::Exit` the classification (or rename the former).

### S-9 · P3 · `crates/holosmith/src/backend/stub.rs:210-217` — `exiting()` is not rustfmt's layout
```
            .exits = Mutex::new(Some(super::ProcessEnd::Exited {
            code,
            stderr: stderr.to_string(),
        }));
```
The struct-literal fields sit at the chain's base indent and the closer one level out; the repo has no `rustfmt.toml` (defaults), and the sibling constructors (`failing_at`/`ignoring_abort`/`failing_session`) are canonical. `scripts/verify.sh:2728` runs `cargo fmt --check` as phase 1.
**Proof:** `cargo fmt --check`. If it is red, the story's own gate stops at its cheapest phase; if it is green, I am wrong about this one line and it should be dropped.

### S-10 · P3 · the generation comparison has no test
`backend/docker.rs`'s only new unit test asserts `supervise` answers `None` for a session nothing launched (`docker.rs:...`). The `if current == Some(generation)` branch (`docker.rs:657-676`) is reachable only by driving `Ladder::climb`, which ADR-0039 itself says `tests/exit.rs` avoids — so the mechanism the ADR calls "one number and one comparison" is proved by argument only. This is the repository's own stated bar ("mutate it and watch the test go red") being missed on the story's central claim.
**Smallest fix:** extract `fn is_current(registry: &HashMap<String, Launched>, id: &str, generation: u64) -> bool` and unit-test it for `Some(1)/1`, `Some(2)/1`, `None/1`. **Proof of the gap:** delete the branch and run `cargo test` — nothing goes red.

### S-11 · P3 · `scripts/verify.sh` — what the two rewritten cases leave behind
- `cleanup()` (`verify.sh:77-86`) only signals the server. `case_agent_launch` removes the container only on its happy path, so any `fail` between `start_serve` and the final `rm -rf "$root"` leaves `s_<id>` running, `agents_<id>` behind and `target/verify-agent-$$/` on the machine — the first Docker gate case that can leak a live container+network on failure (its predecessor left none: the agent step's failure triggered ADR-0015's undo).
- `case_docker_resume`'s new `DELETE` is fire-and-forget: it asserts 202 and then immediately `rm -rf "$root"` (`verify.sh:...`), so the deleter's `remove_worktree`/`remove_session_dir` run against paths that no longer exist, and nothing observes the outcome. `case_agent_launch` does it properly (polls for 404); the resume case should either wait for the 404 or not delete.
**What the two cases still would assert without the story:** nothing spurious — `idle` cannot be reached without a real handshake, and the crash assertion (`'"status":"error"'` after `docker rm --force`) cannot pass without the observer. That part is sound.

---

## Axis 2 — Spec: **PASS WITH FINDINGS**

Acceptance bullets 1, 2, 3, 5, 6 are met with evidence (directory+seed: `agent.rs:105-160` + `the_template_parses_and_carries_no_secret`; environment: `container.rs:446-452` + `the_launch_environment_is_the_agent_directory_and_four_switches` + `docker inspect` in the gate; RPC/ready/`starting → idle`: `agent.rs:376-396`, `provisioner.rs:107-116`; `--approve` + spec line 324 + risk-table row 426 + `get_commands` in `tests/agent.rs`; launch line: `agent.rs:180-200`, asserted by both the unit test and the gate). The findings below are about the two bullets whose *claims* do not hold, plus the two spec sentences the story should have amended.

### P-1 · P1 · `crates/holosmith/src/backend/docker.rs:614-629` + `crates/holosmith/src/exit.rs:168-172, 264-300` — a delete of a live session re-creates the directory it just removed
```
        let _start = self.start_lock(&session.id).await;
        self.remove_container(session).await;        // …the attached process ends here…
        self.remove_network(session).await;
        self.remove_worktree(session).await?;
        self.remove_session_dir(session);            // …and this takes `<data>/sessions/<id>` away
```
`Exits::record` appends *before* it classifies, for every outcome:
```
    pub async fn record(&self, session: &Session, code: i32, stderr: &str) {
        let expected = self.take_flag(&session.id);
        self.append_log(&session.id, stderr);        // exit.rs:170 — unconditional
```
and `SessionLog::append` does `std::fs::create_dir_all(parent)?` (`exit.rs:286`) before opening the file. `delete` never touches the `agents` registry, so the exit the delete itself provokes is still **current**: the watcher taken at the pi step reports `Exited { code: 137, stderr }` → `record` → the deleter's flag makes it `Exit::Expected` → *but the log has already been re-created*. Result: `<data>/sessions/<id>/session.log` reappears after a delete whenever the agent printed anything on stderr during its life (`append` returns early only for empty text, which is why this has not shown up in the gate). That falsifies ADR-0023's "the backend owns the removal", this story's own `delete` doc ("A delete takes both"), `docs/build.md`'s "a `DELETE ?force=true` takes the row, the log, the worktree and the session's own directory", and the gate assertion `[ ! -d "$data/sessions/$id" ]` — for every delete of a **live** session.
Why the gate is green: `case_agent_launch` deletes an `error` row whose exit was already consumed (`docker rm --force` → wait for `error` → DELETE), so the one path it proves is the path without the race; `case_docker_resume` deletes a live session and asserts nothing about the directory.
**Proof (no Docker, ~10 lines):** with the flag set, `fs::remove_dir_all(<data>/sessions/<id>)`, then `exits.record(&session, 137, "anything")`, then assert the directory is gone — it is not. (That is the same shape as `tests/exit.rs:385-415`, which also tells you what **not** to do about it: skipping `append_log` on `Expected` would break `tests/exit.rs:405-414`, "The lines are still kept".)
**Smallest fix:** in `delete`, after `remove_container` and before `remove_session_dir`, take the pending receiver out of `agents` (and/or drop the entry so an in-flight exit answers `NotCurrent`) and await it under a short bound; or remove the registry entry first and let the exit be `NotCurrent`.

### P-2 · P2 · `HolosmithSpec.md:259` vs `HolosmithSpec.md:324` — the spec now contradicts itself about `session.log`
Line 259 (item 15, unchanged by this diff): "… `session.log` holds stderr only." Line 324 (added by this diff, D1): "The launch line in `session.log` records that decision". The code follows 324 (`docker.rs:498-501`), the glossary and `exit.rs:26-32` resolve the conflict by reading 259 as "keeps a *tool's* output out", and `docs/stories/phase-1b-docker-backend.md:169` (HS-033's acceptance) repeats the literal "stderr only" sentence. Acceptance bullet 5 requires the decision to be "documented in the spec"; the half that is missing is *where the line lives*.
**Smallest fix:** amend 259 to "`session.log` holds stderr and the server's launch line, and never a tool's output" (one sentence, no code change).

### P-3 · P2 · `crates/holosmith/src/backend/docker.rs:122-158` — bullet 4's *comparison* has no test
The deliberate choice covers "a second image with a different pi cannot be built here", and ADR-0039 is honest that the mismatch is proved by "`agent::version_sentence`'s unit test and … an image that cannot answer at all". But those two prove the *sentence* and the *docker error arm*; the decision itself —
```
            Ok(Ok(version)) if version == pins::pi_version() => Ok(version),
            Ok(Ok(version)) => Err(agent::version_sentence(…)),
```
— plus its caching (`*cache = Some(answer.clone())`) is untested. A mutation flipping `==` to `!=`, or deleting the cache write, leaves `cargo test` green, so "the agent image's pi version equals the binary's pinned version; a mismatch fails the session" is argued rather than proved.
**Smallest fix (no image needed):** extract `fn assert_pinned(image: &str, found: &str) -> Result<String, String>` from that match and unit-test both arms with two version strings against `pins::pi_version()`.

### P-4 · P3 · `scripts/verify.sh` `case_agent_launch` — the trust assertion is weaker than line 324 claims
`HolosmithSpec.md:324` says "a test proves the mechanism by asking the live agent for its command list and finding the project's own resources there". Only `tests/agent.rs` does that, and it skips without a daemon *and* an image (which the brief accepts). The gate asserts the string `project trust approved` from the launch line, which proves what was *written*, not what pi *did*. Consequence: on a hub or a machine without the image the gate prints `ok`, and the strongest trust claim is unverified. Report-only, since the brief lists the skip as deliberate.

---

## What I could not verify (supervisor to run)
- `cargo test` — I cannot compile; `tests/agent.rs` and `tests/container.rs` need a daemon and `pi-agent:local` (whose pi equals `pins/pi.txt`, per `RESUME.md`'s environment section).
- `cargo fmt --check` (S-9) and `cargo clippy --all-targets -- -D warnings`.
- `scripts/verify.sh` — specifically that `case_agent_launch` reaches `idle` inside its 90 s poll on a cold image, and the P-1 repro above as a new `tests/exit.rs` case.
- Whether `docker rm -f` on an attached `docker run` writes to stderr on this daemon (it decides how often P-1's leftover actually appears — the ordering bug is unconditional, the visible file is not).

## Merge verdict
**OK with notes** — no P0; one P1 (P-1) that is a small, contained ordering change in `DockerBackend::delete` and should be fixed before this story is called done, because it contradicts the delete contract the same diff states three times; the rest are documentation-accuracy and untested-mechanism items (P2) plus report-only notes (P3).
## Resolution

**One redaction in the body above, and nothing else was changed:** the review spelled the pinned pi version
once, in its "what I could not verify" and "smallest fix" lines. `tests/pin_single_source.rs` forbids a second
copy of that literal anywhere in the tree — docs and reviews included — so those two places now say
"`pins/pi.txt`" and "two version strings" instead. The requirement outranks quoting a version number verbatim;
no finding, quote or verdict was touched.

Thirteen of the fourteen findings were fixed in the same session; one was dismissed with the gate's own
output. The one that mattered is **P-1**: the review was right that a delete of a *live* session re-created
the session directory it had just removed, and the reasoning it gave is the reason the fix is where it is.

| Finding | Severity | Outcome |
| --- | --- | --- |
| **P-1** a delete of a live session re-creates `<data>/sessions/<id>/session.log` (and the directory) | P1 | **Fixed.** `DockerBackend::delete` **forgets the launch** before it removes the container, so the exit its own removal provokes answers `ProcessEnd::NotCurrent` and nothing is appended or classified. That replaces the delete's expected-stop write, which was the wrong mechanism (see S-5). Proved by `an_exit_is_news_only_for_the_launch_that_is_current`'s third case (a forgotten launch) and by `case_docker_resume`, which now deletes a **live** session and asserts the directory is gone *and stays gone*. |
| **P-2** the spec contradicts itself about `session.log` (item 15 "stderr only" vs D1's launch line) | P2 | **Fixed** in `HolosmithSpec.md` item 15: "holds stderr (the kept tail of it) and the server's own launch line, and never a tool's output". The code was already what D1 says, and `exit.rs`/`GLOSSARY.md` now say the same thing (see S-4). |
| **P-3** the version *comparison* had no test, only the sentence | P2 | **Fixed.** The comparison is `agent::version_check(image, found)` and `the_pinned_version_is_the_one_the_image_must_run` exercises both arms, deriving the "other" version from `pins::pi_version()` so the test cannot become a second copy of the pin. The per-process **cache** stays untested — it would need a second image — and ADR-0039 now says so rather than implying the whole assertion is proved. |
| **P-4** the gate's trust assertion is weaker than the spec sentence | P3 | **Fixed in the spec**, which now says the mechanism is proved by "a test that needs a daemon and the agent image" (`tests/agent.rs`'s `get_commands` assertion). The gate still asserts the launch line, which is what the *artifact* can show without a live project extension. |
| **S-1** `backend/docker.rs`'s module doc still said the pi step refuses | P2 | **Fixed**: all four steps are real, the turn's five operations are what refuses, and the module now states the registry/generation rule a reader of `delete` and `supervise` needs. |
| **S-2** `clear_expected_stop`'s doc said HS-031's ordering constraint was still open | P2 | **Fixed**: it says how the constraint is kept (a replaced launch answers `NotCurrent`), and that the ladder needs no wait. |
| **S-3** `agent.rs` pointed at a trust argument that was not in the Docker backend's module doc | P2 | **Fixed**: the comment cites ADR-0039's "Project trust is `--approve`" section and says why the alternative is not this build's choice. |
| **S-4** "the whole of stderr goes to `session.log`" was no longer true (the reader keeps 64 KiB) | P2 | **Fixed** in `exit.rs` (both sentences) and `GLOSSARY.md`'s **Session log** entry: the log holds the *kept tail*, drained from the pipe and bounded, and the row's reason is five lines of it. |
| **S-5** the delete's flag write was justified by a transition the table does not have | P2 | **Fixed** by removing the write: the delete does not need a flag, and the comment/ADR now give the true reason (the backend forgets the launch). `Stopper` keeps the flag for a stop of a session that will still exist. |
| **S-6** two dead public accessors (`Exits::session_log`, `Exits::log_path`) | P3 | **Fixed**: both removed. The backend has its own `SessionLog`, and the log's path has one spelling (`exit::session_dir(…).join(LOG_FILE)`), which is what the test and the backend both use. |
| **S-7** `request`'s *write* had no bound, only the reply | P3 | **Fixed**: the lock, `write_all` and `flush` are inside the same `tokio::time::timeout` as the reply, so a process that stops reading its stdin fails the command instead of holding the mutex for ever. (Unreachable for the handshake's tiny payload, which is why it was not a bug yet — but it is the seam HS-032 widens.) |
| **S-8** `GLOSSARY.md` cross-referenced a term with no entry, and `Exit` named two ideas | P3 | **Fixed**: a **Launch generation** entry, and `agent::Exit` is renamed `agent::Ended` — "how the process ended, as it reported it" — with its doc pointing at `exit::Exit`, "what that means for the row". |
| **S-9** `stub.rs`'s `exiting()` was not rustfmt's layout | P3 | **Dismissed**: `cargo fmt --all --check` is green (it is the gate's first phase), so rustfmt accepts the layout the review read as wrong. The diff was reformatted anyway; nothing changed. |
| **S-10** the generation comparison had no test | P3 | **Fixed**: `an_exit_is_news_only_for_the_launch_that_is_current` proves all three branches without Docker, with a fake agent that ends when a **file** appears so the comparison is never raced. Mutating the comparison or deleting `forget_agent` reddens it. |
| **S-11** the two rewritten gate cases could leak a container on failure / fire a delete at a removed root | P3 | **Fixed**: the script's `EXIT` trap now removes a `docker_session_id` the Docker cases set (the only hook `fail` leaves), and `case_docker_resume` waits for its `404` and asserts the session directory is gone instead of removing its scratch root underneath a running task. |

What the review got right that the plan had not: **P-1** is exactly the class of bug the plan's "the delete
marks its own removal expected" line was meant to prevent, and the fix is smaller than the mistake. It also
found that the story's own documentation repeated a false claim (S-5) in two places, which is the pattern the
repository's landmine list warns about: a justification written once and copied is not checked by anything.

Re-run after the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (36
binaries, no failures), and `scripts/verify.sh` — 50 cases, 407 assertions.
