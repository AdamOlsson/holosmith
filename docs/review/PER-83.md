# Review — PER-83 (HS-051: `drain` over the admin socket)

Reviewed at `1892705` against the fixed point `e3d39ff`, by a read-only reviewer running
`~/.agents/skills/code-review/SKILL.md`. The two axes below are the reviewer's, verbatim.

## Review

I've read the brief, the skill, the full diff, and the code at HEAD. Here is the two‑axis review.

---

## Standards

**S1 — P2 — `crates/holosmith/src/drain.rs:296-301` (+ `:266-272`) — a database read failure in the wait emits *two* terminal `result:` lines and the wrong one wins.** The error arm reports its own terminal line and then returns `false`:

> `report.send(&format!("note: the sessions could not be read ({error})")); report.result(Ending::Refused); return false;`

…and `run` treats *any* `false` as expiry:

> `if !self.wait_for_turns(deadline, report).await { report.send(&format!("note: the wait ran out after {} with a turn still running; …")); report.result(Ending::Expired); return Ending::Expired; }`

ADR-0060 D3 says the wire is "one request line, progress lines, and one `result:` line", and the error arm's own comment says "Reporting it as 'expired' would say the wait ran out, which is **not what happened**". The client is last‑line‑wins (`commands/drain.rs:111-113`: `if let Some(word) = line.strip_prefix("result: ") { ending = … }`), so the operator sees `result: refused`, then a false "the wait ran out after 10m with a turn still running", then `result: expired`, exit 1. No test covers the arm (`grep` in `tests/drain.rs` finds no `Refused` from the sequence). It should return a three‑valued outcome (or the `Ending` itself) from `wait_for_turns` and let `run` report exactly one terminal line.

**S2 — P2 — `crates/holosmith/src/drain.rs:336-345` with `:273-279` — a failed read of the rows to stop still ends `result: drained`, exit 0.** `stop_idle` swallows the error into a sentence:

> `Err(error) => { tracing::error!(…); return "no session could be read".to_string(); }`

and `run` then unconditionally reports `report.result(Ending::Drained); Ending::Drained`. ADR-0060 D3: "0 must mean 'the machine is quiet, go' and 1 everything else" — here 0 means "I could not look, restart anyway", with idle sessions still live. The same arm also mislabels a write failure: `Err(error) => { … "an idle session left idle before the drain stopped it"; continue; }` catches `StateError::Repository` (`sessions/state.rs:285`) as well as `Illegal` the race, so a database failure reads in the log as "the session took a prompt". The dashes/`stopped:` count should distinguish "could not read" from "stopped zero", and a read failure should be `Ending::Refused`.

**S3 — P3 — `crates/holosmith/tests/drain.rs:617` — the steer assertion cannot observe what its comment claims.** The comment says "a steer on the surviving running session is **still delivered**", the assertion is `assert_ne!(response.status, 503, "{}", response.body)`. It passes on `500 internal` (`sessions.rs:803`) and on `409 session_idle`; it only rules out the drain's own code. The row is `running` at that point (`abort_grace` is 10 s, `config/mod.rs:272`, so the ladder has not moved it), so `assert_eq!(response.status, 202)` is the assertion the comment promises — and it would still fail if `steer` gained a drain guard.

**S4 — P3 — `crates/holosmith/src/commands/serve.rs:356-358` — the comment documents a distinction the code does not make.** Comment:

> `// The reason is a `warn` when the directory is not usable and a `debug` when another server owns the`
> `// socket: the first is a machine this run cannot drain, the second is a second server nobody meant to`

The arm issues one unconditional `tracing::warn!` (`:360-365`) for every `listen` failure, including `"… is already answered by a running server"`. ADR-0060 D2 says a live socket is "one `warn`", so the code is right and the comment is the leftover; delete the `debug` claim or split the arm.

**S5 — P3 — `crates/holosmith/src/drain.rs:237` and `:454` — two public items with no caller.** `pub fn control(&self) -> &Control` is documented as having none ("for a caller that wants to read it (the handlers reach the state's own copy)"), and `impl Default for LineSink` is likewise unused (every caller uses `LineSink::new()`); `grep` over `crates/` finds no `.control()` and no `LineSink::default()`. Speculative generality — delete both.

**S6 — P3 — `docs/adr/0060-…md:177` and `docs/notes.md:1749,1756` — three checkable miscounts.** The ADR says "three of `verify.sh`'s own boots" did not pass `--admin-dir`; the diff adds it at **four** sites (`scripts/verify.sh:5149`, `:6454`, `:6561`, `:6987`). `notes.md:1749` says "**six** such spawns (`tests/db_migrations.rs`'s three, two direct ones in `tests/serve_cli.rs`, three of `verify.sh`'s own boots)" — its own parenthesis enumerates eight. And `notes.md:1756` says "The stub's turn is **ten** 200 ms frames"; `stub.rs:301-348` scripts twelve entries, each preceded by one `sleep(step_delay)` (`stub.rs:408`), which is why `stub.rs:37` calls it "a turn's dozen frames". The `--wait 200ms` conclusion is unaffected; the numbers are.

---

## Spec

**P1 — `crates/holosmith/src/commands/drain.rs:83` with `:103-108` — a 5‑second read timeout turns a healthy, quiet drain into "the server stopped answering", so `holosmith drain --wait 1m` fails whenever the turn outlives five seconds.** The client sets a per‑read timeout on the stream:

> `pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);` … `let _ = stream.set_read_timeout(Some(ANSWER_TIMEOUT));`

and treats any read error as fatal:

> `let line = line.map_err(|error| format!("{} stopped answering mid-drain ({error}); …"))?;`

But the server is *silent* between progress changes by design — `drain.rs:303`: `if reported != Some(counts.running) && counts.running > 0 {` — so for one turn taking 30 s the only lines are `state: draining`, `waiting: 1 turn still running`, then nothing until the turn settles. `SO_RCVTIMEO` fires at 5 s, `read_line` yields `Err(WouldBlock)`, and the command exits 1 blaming the socket. This breaks the story's own demo (*"start a turn, run `holosmith drain --wait 1m`"* — acceptance bullet 4 and the Demo line), spec item 10 step 3's promotion, and `docs/build.md`'s by‑hand `holosmith drain --wait 2m && echo quiet || echo not-quiet`. It is invisible to the gate because `--wait 200ms` and `30s` bracket a ~2.4 s stub turn, and to `tests/drain_cli.rs` because every fake server (`answer_once`) writes all its lines before the client reads. Smallest fix: keep `ANSWER_TIMEOUT` for the *first* line only (`stream.set_read_timeout(None)` once a line has arrived, or loop on `WouldBlock` while an overall `wait + grace` bound holds) — the drain's real deadline is already the server's `--wait`. Add a `tests/drain_cli.rs` server that stays silent for >5 s between `state:` and `result:`; that test fails today.

Everything else in the acceptance is met and measured: socket‑only and unreachable over HTTP (`drain::listen` + `tests/drain.rs:649` + `verify.sh:3960-3990`), 503 `draining` on create/prompt/resume with reads/abort/steer/delete open (`tests/drain.rs:543-620`, ADR‑0060 D4's table matched row for row), wait‑then‑stop with `server_restart` and `expected_stop` (`verify.sh:4074-4102`), non‑zero expiry that leaves the flag on (`tests/drain.rs:296`), and bullet 5 via ADR‑0053's reconcile (`verify.sh:4149-4226`).

---

## What is right

- The refusal list and its ordering match ADR‑0060 D4/D5 exactly, and the create's ordering (key lookup → refusal → cap) is the code's own `create_locked` (`http/sessions.rs:545-559`), measured by `tests/drain.rs:623` plus the pre‑existing `tests/worktree.rs:1141`.
- The stop phase is honest: `Transitioner::apply(Trigger::Drain, Some(SERVER_RESTART))` then `Stopper` in a spawned task, `stopped += 1` only after the transition succeeds (`drain.rs:372`), and the read/move race is logged at `info` rather than `unwrap`ped.
- The socket is the right shape: `symlink_metadata` + `is_socket()` before anything is unlinked, probe‑refused ⇒ replace, connected ⇒ refuse, a regular file ⇒ a sentence (and the test asserts the operator's bytes survive, `tests/drain.rs:474-480`); `MAX_REQUEST` via `take`, `REQUEST_TIMEOUT`, and no write to a socket the client closed (`connected`).
- Both gate cases SKIP only on a missing `python3`; every assertion I traced fails if its feature is deleted, the cases own their boots under `$WORK`, and both restore the shared boot and leave the shared rows alone.
- `docs/stories/phase-2-deployable-mvp.md` is untouched; the only `docs/stories/` edit is one ADR‑index line in `README.md`, which is where ADR‑0057/0058/0059 are recorded too.
- The landmine fixes hold: every test and gate boot that actually binds now passes `--admin-dir` (`tests/db_migrations.rs` ×3, `tests/serve_cli.rs` `Server::start` + 2, `verify.sh` ×4); the five `serve` spawns left without it all fail before the async body (`serve_cli.rs:499,515,526,551,577,621`), and the earlier `Uninstall` help‑text regression is repaired (`cli.rs:85`).

**Standards: 6 findings (2 P2, 4 P3); worst = S1/S2, a false `drained`/exit 0 and a two‑line terminal report.** **Spec: 1 finding (P1); worst = the 5 s read timeout.** The two axes are independent: the change implements the story faithfully, and the one thing that breaks the demo is a client timeout, not a spec misreading.

verdict: fix-first
## Resolution

Every finding is fixed; none is dismissed. The tests named below are the record that each fix can fail: the two
`tests/drain.rs` arms were written first and one of them caught the first attempt at S1 (`run`'s `Unreadable` arm
returned `Refused` without sending a terminal line, which the test failed on before the fix went in).

| # | Severity | Outcome | Evidence |
| --- | --- | --- | --- |
| Spec P1 | P1 | **fixed** | `commands/drain.rs`'s read loop no longer treats a timeout as a dead socket: the **first** line is bounded by `ANSWER_TIMEOUT` (is anything there at all) and everything after it by `--wait + ANSWER_TIMEOUT`, which is the deadline the operator asked for. Three new unit tests in that module (`a_silent_gap_longer_than_the_bound_is_not_a_dead_server`, `a_server_that_never_says_anything_is_refused`, `a_connection_closed_before_the_result_is_a_failure`) pin both arms. Measured against the reviewer's repro — a fake socket that says `state:`/`waiting:` and then nothing for 6 s — the command printed `waiting: 1 turn still running`, then `stopped: …`, then `result: drained` and exited **0**; before the fix the same repro printed `stopped answering mid-drain (Resource temporarily unavailable)` and exited **1**. ADR-0060 D3 records the rule so a later reader does not re-tighten it. |
| S1 | P2 | **fixed** | `wait_for_turns` now answers a three-valued `Wait::{Quiet, Expired, Unreadable}` and `run` reports exactly one terminal line. `tests/drain.rs::a_wait_that_cannot_count_the_turns_is_one_refused_line` drops the `sessions` table and asserts one `result:` line, the word `refused`, the `note: the sessions could not be read` sentence, and the absence of any "the wait ran out" line. |
| S2 | P2 | **fixed** | `stop_idle` returns `Result<u64, ()>`: a read failure or a `Repository` write failure ends the sequence at `result: refused` (exit 1) instead of `drained`, and the count is never printed when there is none. The `Illegal`/`UnknownSession` race is matched separately from `Repository`, so a database failure no longer logs as "the session took a prompt". `tests/drain.rs::a_stop_phase_that_cannot_list_the_rows_is_refused_rather_than_quiet` breaks the schema in the one way `counts_by_status` survives and `list` does not (renaming a `COLUMNS` column) and asserts `refused` with no `stopped:` line. |
| S3 | P3 | **fixed** | `tests/drain.rs`'s steer assertion is now `assert_eq!(response.status, 202)`, with a comment naming the two wrong answers it would have accepted (`409 session_idle`, `500 internal`). |
| S4 | P3 | **fixed** | `commands/serve.rs`'s comment now says what the arm does — one `warn` for every reason, with the sentence telling them apart — and drops the `debug` claim the code never made. |
| S5 | P3 | **fixed** | `Drain::control()` and `impl Default for LineSink` are deleted; neither had a caller. |
| S6 | P3 | **fixed** | `docs/adr/0060-…md` D9 now counts **eight** spawns across three files and says why the mistake stayed invisible (`start_serve` already had the flag); `docs/notes.md` says eight where it said six, and twelve 200 ms frames (~2.4 s) where it said ten. |

No spec finding was dismissed. The one place this review changed the design rather than the words is Spec P1: the
gate could not see it (every fake socket wrote all its lines before the client read, and both gate cases bracket a
~2.4 s stub turn), so the fix carries unit tests in the client's own module and a landmine in `docs/notes.md`.
