# Review — PER-61 (HS-029)

Fixed point `b009320`; reviewed at `e826539` by a read-only `reviewer` child running the `code-review`
skill on both axes. The body below is verbatim; the `## Resolution` table at the end is the
orchestrator's.

---

## Review

Reviewed `b009320..e826539` from `/tmp/holosmith/reviews/PER-61-diff.txt` against `HolosmithSpec.md` item 6, `docs/stories/phase-1b-docker-backend.md` HS-029, `docs/adr/0037-a-per-session-start-lock.md`, `GLOSSARY.md`, `docs/build.md`, plus the surrounding source.

**Verdict — Spec: PASS with findings.** All four HS-029 acceptance lines are implemented at the seam they name (`rm -f` in one function that every start path calls; `stop` is `docker stop --time 10`; the stop→remove→resume test exists; the per-session lock exists), `case_docker_resume` is registered in `serve_cases`, ADR-0037 and `docs/build.md` and `GLOSSARY.md` are updated, and nothing outside the story's scope was built (HS-030's flag and classification are correctly left out). Findings below are precision problems, not missing scope.

**Verdict — Standards: PASS with findings.** No P0. One P1 (a test that reintroduces the flake its own doc comment measures), two P2 (a documented invariant that does not hold; a `bool` that cannot distinguish absence from failure, contradicting the same file's own teardown), three P3.

### P1

1. **`crates/holosmith/tests/container.rs:676-690` — the planted-leftover step races `--rm`'s removal, which the test itself documents.** After the second `backend.stop(&session)` (line 678) the test immediately runs `docker run` under the same `s_<id>` (line 680) and asserts it succeeded (line 685). `wait_gone` exists two functions up (`:604-611`) precisely because "the daemon does that *after* `docker stop` has answered … this test did [fail], one run in three". The first stop in this same test waits (`:657-660`); the second does not, so the planted `docker run` intermittently fails with "the container name is already in use" — the exact measurement in ADR-0037:9-14.
   *Smallest fix:* one line after the second stop, before the planted run: `assert!(wait_gone(&docker, &container.name), "`--rm` left the container behind for two seconds");`.

### P2

2. **`crates/holosmith/src/backend/docker.rs:80-86` — the map sweep never runs; the doc comment and the ADR claim it does.** At line 84 the strong count is at least 3, not 2: the map's entry (1), the local `lock` (2, alive until the function returns, since line 84 still borrows it) and the `OwnedMutexGuard`'s own `Arc` (3, `lock_owned` holds a clone). So `Arc::strong_count(&lock) <= 2` is never true and `starts.remove` is dead code: the map grows one entry per session this process ever starts. Contradicted by `docker.rs:66-69` ("the map is swept rather than grown by one entry per session"), `docs/adr/0037-a-per-session-start-lock.md:39-41`, and `docs/build.md:384-389`'s framing of the ADR.
   *Smallest honest fix:* do **not** just change `<= 2` to `<= 3`. Removing the entry while this call still holds the guard lets a third caller mint a fresh mutex and enter the critical section concurrently with this one — the race the lock exists to prevent. Either delete the sweep and the two claims (an empty `tokio::sync::Mutex` plus a 16-byte id per session is not a leak worth a racy registry), or hold `Weak<tokio::sync::Mutex<()>>` in the map, `upgrade()` on acquire (re-inserting on `None`), and prune dead weaks during that same locked section.

3. **`crates/holosmith/src/docker.rs:112-114` with `crates/holosmith/src/backend/docker.rs:398-412` — `stop` cannot tell "no container" from "the daemon failed", and says the first while doing the second.** `succeeds` answers `false` for a missing container *and* for a wedged daemon, a missing CLI or a permission error (`docker.rs:72-79`). `DockerBackend::stop` then emits `"there was no container to stop"` and returns `Ok`, so `Ladder::rung_two`'s `warn` (`src/ladder.rs:254-262`) is unreachable for the Docker backend and a forced abort leaves the container alive under a `stopped` row with a debug line asserting the opposite. The backend's own teardown 120 lines above (`docker.rs:278-306`) documents and implements the three-way answer: "a teardown that cannot say which happened is a teardown nobody can debug".
   *Smallest fix:* `pub fn stop_container(&self, name: &str) -> Result<bool, DockerError>` — `Ok(true)` on success, and absence distinguished without parsing stderr by `if !self.container_exists(name) { return Ok(false) }` then `self.run(&["stop", "--time", STOP_SECONDS, name]).map(|_| true)`; in `stop`, propagate `Err` and keep the `debug!` only for `Ok(false)`.

### P3

4. **`crates/holosmith/src/backend/docker.rs:278` — one name, two meanings, in the story that separates them.** `DockerBackend::stop_container` force-*removes* (`rm --force`), while the new `Docker::stop_container` (`docker.rs:112`) *stops*. A reader following `stop` (`:398`) into `stop_container` gets the teardown. *Fix:* rename the backend's to `remove_container`, matching `remove_network` directly beneath it.

5. **`crates/holosmith/tests/container.rs:796-812` — the race test does not cover the path production takes.** It shares one `Arc<DockerBackend>`; production gives each task its own clone (`src/provisioner.rs:59` consumes `self`, `spawn(self)`, holding a cloned `Backends`), and only `starts` being an `Arc` makes the lock shared. A regression to a per-instance map would still pass this test. *Fix:* `let backend = (*backend).clone();` inside the loop instead of `Arc::clone`.

6. **`crates/holosmith/src/backend/docker.rs:370-373, 476-490` — the lock covers starts only; teardown force-removes without it.** `delete` and `undo(Step::Container)` call `stop_container` (the `rm --force` one) with no start lock, so a delete concurrent with an in-flight start can remove the network the start's `docker run` needs, or remove the container the start just created. Outside HS-029's acceptance ("two *starts*"), so report-only. *Fix if taken:* `let _start = self.start_lock(&session.id).await;` at the head of `delete` and of `undo(Step::Container)`.

7. **`docs/adr/0037-a-per-session-start-lock.md:37-38` — a false premise for the lock's placement.** "`resume` has a different caller from `create` (the provisioner, not the request)": the provisioner calls `provision(Step::…)`, never `SessionBackend::resume`; `DockerBackend::resume`'s only caller in the tree is `tests/container.rs:751`. *Fix:* drop the parenthetical, or state that the lock is in the backend because the resource is the backend's and the transaction spans two daemon calls — true without the caller claim.

### Verified by reading
- `rm -f` precedes `run` in one place (`backend/docker.rs:217-221`) and both start paths reach it: `provision_container` (`:199-207`) and `resume` (`:420-431`). No other code path calls `run_container`.
- `resume` takes the start lock, re-creates the network, starts the container, touches no worktree (matches deliberate decision 2).
- `stop` uses `--time 10` (`docker.rs:21, 113`), matching spec item 6's `docker stop -t 10`; `--rm` is already in `container::run_args` (`container.rs:375`), and `stop` performs no removal of its own.
- Acceptance 3's test asserts the branch head *and* the container-written file on the host (`tests/container.rs:752-770`), i.e. it would fail if the worktree were not a host directory.
- `case_docker_resume` is in `serve_cases` (`scripts/verify.sh:329`), skips on no daemon/no image like `case_docker_container`, and its `error`-row resume is legal: `live_count` counts only `starting|idle|running` (`src/sessions/repository.rs:189-201`) so `max_live_sessions = 1` does not refuse it, and `sessions::resume` accepts `Stopped | Error` (`src/http/sessions.rs:838`).
- The refusal test was narrowed correctly: `stop`/`resume` removed from the list, remaining five still named (`backend/docker.rs:516-540`).
- `STOP_SECONDS` is used; `docs/stories/README.md` count moved 36→37 with the new ADR linked; the `GLOSSARY.md` **Start lock** entry matches the implementation's shape and `_Avoid_` list.
- No dead code or unused imports introduced at the seams I traced; `Provisioned`/`Step` handling unchanged.

### Could not verify without running
- That `docker run` really fails inside the ~ms window after `docker stop` returns, i.e. that finding 1 is a ~1-in-3 flake rather than a non-event (ADR-0037 asserts the measurement; I have only its word).
- That the sweep's `strong_count` is 3 rather than 4 in the tokio version pinned — my count assumes `OwnedMutexGuard` retains an `Arc` (true of every tokio I know) and that no post-await copy of the future holds one.
- That the 8-way race test fails with the lock removed on this machine (ADR-0037 claims 3/3 runs); and that the lock actually serialises under `cargo test --test container` parallelism.
- `cargo test`, `cargo clippy`, `scripts/verify.sh` (needs a daemon and the `pi-agent:local` image), and whether `case_docker_resume`'s 30-second polls are long enough for `docker run` on a cold image. Supervisor should run: `cargo test --all` and `scripts/verify.sh --serve` (or the full script) on a machine with the daemon and image.
---

## Resolution

| Finding | Verdict | What changed |
| --- | --- | --- |
| **P1-1** the planted-leftover step races the `--rm` removal the test itself documents | **Fixed** | The second `stop` is now followed by `wait_gone(...)`, like the first. The reviewer is right that this was the exact flake one line above it. |
| P2-2 the map sweep never runs (`strong_count` is 3, not 2) and removing the entry would let a third caller mint a fresh mutex and enter concurrently | **Fixed by deleting the sweep** | The reviewer's second option (a `Weak` registry) is right in principle and not worth its complexity here; the first (no sweep) is what the code does now, with the reason written down: an unlocked `tokio` mutex plus a 16-byte id per session, in a process that restarts long before it matters. The three places that claimed a sweep (`backend/docker.rs`, ADR-0037, and the ADR bullet in `docs/stories/README.md`) no longer do. |
| P2-3 `stop` cannot tell "no container" from "the daemon failed", and said the first while doing the second — which would leave `Ladder::rung_two`'s `warn` unreachable | **Fixed** | `Docker::stop_container` answers `Result<bool, _>`: it asks `container_exists` first (so absence is a fact rather than Docker's English) and `DockerBackend::stop` propagates the error, keeping the `debug` line only for `Ok(false)`. |
| P3-4 two functions called `stop_container`, one of which force-removes | **Fixed** | The backend's is `remove_container`, matching `remove_network` beside it; `Docker::stop_container` is the graceful stop. |
| P3-5 the race test shared one `Arc<DockerBackend>`, so a regression to a per-instance map would still pass | **Fixed** | Each task gets its own **clone**, which is what the provisioner does — the clone is the thing that must share the lock. |
| P3-6 a teardown force-removes without the start lock | **Fixed** | `delete` and `undo(Step::Container)` take the same lock: a delete concurrent with a start could otherwise remove the network between the start's `network create` and its `docker run`. Outside the story's acceptance, taken because it is the same race from the other side. |
| P3-7 ADR-0037's "`resume` has a different caller from `create` (the provisioner, not the request)" is false — the provisioner calls `provision`, never `resume` | **Fixed** | The parenthetical is gone; the reason is the resource and the two-call transaction, which is true without it. |

The reviewer's four "could not verify without running" items were run afterwards: `cargo test` (255 lib +
35 integration binaries), `cargo clippy -D warnings`, `cargo fmt --check`, `scripts/verify.sh` (**49 cases,
367 assertions, ok**), `cargo test --test container` five times in a row (green each time, and green three
more times after the P1 fix), and the lock's removal was re-proved to make the race test fail. The
`case_docker_resume` poll budget (300 × 0.1 s) is ample: the whole case takes ~2 s here.

One finding the reviewer did not raise and the gate did: `RESUME.md`'s new §2 text spelled the pinned pi
version, which `tests/pin_single_source.rs` forbids outside `pins/pi.txt`. Rewritten to name the file
instead of the number.
