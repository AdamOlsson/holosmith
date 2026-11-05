# Review — PER-64 / HS-032 (RPC mapping: message deltas)

Saved verbatim from a reviewer child, with two editorial elisions the repository's pin guard requires: the pi
release literal appears only in `pins/pi.txt`, so the pinned version and a documentation path under
`~/.pi/agent/install/releases/` are spelled `<version>` here. The file also opens with this heading, which the
review itself did not have.

## Review

**Artifact note:** `/tmp/holosmith/reviews/PER-64-diff.txt` is one commit behind the working tree. The tree also carries `crates/holosmith/tests/agent.rs::the_turns_commands_reach_pi_and_a_refusal_is_pis_sentence`, `backend/docker.rs::every_command_gets_an_id_of_its_own` and an ADR-0040 rewording (`pins/pi.txt` instead of the pinned literal). I reviewed the tree, not just the artifact. `watchdog_diff` returned "reviewer-launch HEAD changed or is unavailable", so I used the supplied artifact plus source reads.

### Correct (verified by reading)

- The pure mapping is genuinely pure and the fixture runs through production code: `Mapping::map` reads no clock/DB/agent (`rpc.rs:120-241`), and `tests/rpc.rs:182-345` hands the committed fixtures to the same `Mapping` `start_turn` builds at `backend/docker.rs:663-664`. Acceptance bullet 5 is met with real records (system prompt, user message, `agent_start`/`agent_end` all present and declined).
- Acceptance bullets 1 and 3: `text_delta`/`thinking_delta` → three-key deltas with a minted id and the kind (`rpc.rs:158-176`), and `message.end` carries `usage`/`stop_reason`/`tool_calls`, with `usage` *omitted* rather than zeroed when pi reports none (`rpc.rs:340-350`; unit test `a_message_with_no_usage_ends_without_the_key`). The `stop_reason`/`tool_calls` addition breaks no consumer: the repository reads only `usage`/`message_id` (`events/repository.rs:22-36`), the page parses `message.end` for those two and ignores the rest (`web/src/transcript.ts:126-145`).
- Subscribe-before-write is correct and tested: `agent.records()` at `docker.rs:632` precedes the prompt write, and the reader publishes every parsed non-`response` record (`agent.rs:558-563`).
- The `Lagged`/`Closed` split is right about the real hazard — the `Agent` value keeps a sender alive in the registry, so "the process is gone" has to be the reader's own `None`, not channel closure (`agent.rs:255-282`; the reader's final `let _ = records.send(None)` at `agent.rs:611-616`).
- Phase rule respected: no API/UI change; `Session` untouched; a new kind was not invented — `message.end` was extended additively.
- No cross-session leak: the broadcast lives on one session's `Agent`, the mapping is constructed with the row's id (`docker.rs:663`), and the frames carry it.
- `accept`/`turn_failure`/`live` (`docker.rs:252-269`, `849-870`) are small, well-named seams; the four operations are one `request` each with a bounded timeout, and rung one of the ladder is bounded by `bounded(deadline, …)` (`ladder.rs:183`), so the 30 s `ABORT_TIMEOUT` cannot outlive `abort_grace`.

---

### Finding: P1 — the "removal supersedes the launch" fix is on a call site the resume path never reaches (Standards + Spec)

`docs/adr/0040-the-mapping-layer.md:105-116`, `crates/holosmith/src/backend/docker.rs:504-513` and `docs/build.md:216-218` all say the resume flake is fixed because *the removal* supersedes the launch first. The code puts `self.supersede_agent(&session.id)` inside `DockerBackend::remove_container` (`docker.rs:514`). But the removal the ADR describes — "`start_container` begins with `docker rm -f s_<id>` … which ends the attached `docker run` of the session's previous agent" — is **not** that function: `start_container` calls `Docker::remove_container` inline (`docker.rs:381-385`), and `DockerBackend::remove_container` has exactly two callers, `undo(Step::Container)` (`docker.rs:609`) and `delete` (`docker.rs:784`, which calls `forget_agent` on the line before, so `supersede_agent` finds no entry and is a no-op there).

The resume path is `http::sessions::resume` → `provisioner().spawn` (`http/sessions.rs:834-878`) → `for step in Step::ALL` (`provisioner.rs:78`) → `provision` (`docker.rs:591-596`) → `provision_container` (`docker.rs:346-353`) → `start_container`. So on a resume the previous launch's generation is still current when the `rm -f`'s exit arrives, and `supervise` compares it to the registry at *that* moment (`docker.rs:791-830`) before `start_container` inserts the new generation (`docker.rs:400-419`). That is precisely the sequence the ADR says was measured "about one run in two".

Why it matters: this is the one piece of the story the brief calls in-scope because a *demo* found it; as written, the flakiness the ADR documents is untouched and three documents assert it is fixed. It also means `docs/build.md:216-218`'s claim about what `case_docker_turn` proves is unsupported.

Smallest fix: call `self.supersede_agent(&session.id)` at the top of `start_container`, before the `docker rm -f` closure (`docker.rs:377-385`) — that is where its own doc's hazard ("a removed entry restarts the count at 1") actually arises — and keep the call in `remove_container` for the undo/delete paths. Add a test: launch nothing, insert a `Launched` with generation 1, call `start_container` against a stub that fails at the next step, and assert the registry's generation is 2 and `exit` is `None`.

### Finding: P2 — pi's `turn_start`/`turn_end` are per *assistant response*; the transcript's `turn.*` is per *prompt* (Spec)

`rpc.rs:124-125` maps pi's boundaries straight onto `TURN_START`/`TURN_END`. pi's own table (the authority this repo cites, `agent.rs:12`) defines them as "One assistant turn started" and "One assistant response and its resulting tool calls finished … A turn is one assistant response plus any tool calls and tool results produced by that response" (`~/.pi/agent/install/releases/<version>/node_modules/@earendil-works/pi-coding-agent/docs/json.md:57-60`) — the same document says `agent_start`/`agent_end` can repeat for retries, steering and follow-ups.

But the transcript's turn is the operator's prompt: the stub emits exactly one pair per prompt (`backend/stub.rs:309,354`), `turns_before` counts `turn.start` rows to number turns (`events/repository.rs:331-338`), and the client increments `turn` on each `turn.start` and `settled` on each `turn.end` (`web/src/transcript.ts:235-241`), rendering "turn N settled" (`web/src/main.ts:493-498`) and feeding `budgetNotice` (`web/src/transcript.ts:341-344`).

Consequence: a provider retry — reachable today — or, from HS-033, any tool-using turn, emits N boundary pairs for one prompt, so the log's turn numbers diverge from the stub's, the page says "turn 1 settled" while the row is still `running`, and `settled` over-counts. Today's fixtures cannot see it because each has exactly one assistant message. ADR-0040 never discusses the boundary frames at all, so this is a decision made in code and argued nowhere.

Smallest fix: emit `TURN_START` once per `pump` and `TURN_END` when the run settles (pi's pairs then only *locate* the run's edges), record it in ADR-0040, and add a fixture arm with two assistant responses (a tool round trip) so the test can distinguish.

### Finding: P2 — the record channel is lossy, and the acceptance line says every delta must reach the writer (Spec)

`agent.rs:224-231` and `:558-563`: `broadcast::Sender::send` is non-blocking; a full ring drops the oldest. `pump`'s `Received::Lagged` arm (`rpc.rs:296-303`) logs a `warn` and carries on, so text silently disappears from the transcript. Acceptance: "Every delta reaches the writer; ordering is preserved end to end" (`docs/stories/phase-1b-docker-backend.md:150`). The writer is bounded and `append` awaits (`events/mod.rs:104-115`), so a slow commit is exactly what makes the pump fall behind; ADR-0040:89-92 records the trade-off but treats "fail the turn" as the only alternative, and no test exercises the arm.

Smallest fix: since one subscriber is the design, hand the turn a bounded `mpsc` that the reader `send().await`s — that is backpressure (ADR-0013's own rule for the writer) instead of loss, and the `Lagged` arm disappears. If loss is genuinely chosen, say in ADR-0040 that this is a deliberate deviation from the acceptance line and cover the arm in a test.

### Finding: P2 — a doc comment that now claims the opposite of the code (Standards)

`agent.rs:507-510` still says of `read_records`: "A record that is not JSON, **or is a record type this build does not map yet (session events are HS-032's), is counted and skipped**". The diff changed exactly that (`agent.rs:558-563` publishes them; only unparsable lines and orphan responses move `skipped`). The module doc (`agent.rs:1-26`) never gains the new seam either — no mention of `Received`, `AgentRecords` or `RECORDS_BUFFER`, though it lists the module's other three rules. HS-035 will read this comment as the contract.

Smallest fix: rewrite that paragraph ("non-`response` records are published to a turn through `RECORDS_BUFFER`; only what cannot be parsed or routed is counted") and add one bullet to the module doc.

### Finding: P2 — the wire shape of `message.end` is documented in the code that owns the names, and it was not updated (Standards)

`events/mod.rs:44-47` — the constant's doc comment *is* this repo's spelling of the payload ("A message finishing: `{"message_id", "usage": {"in", "out"}}`"). This diff makes `stop_reason` and `tool_calls` part of the frame on every message (`rpc.rs:196-205`), and leaves the comment at two keys.

Smallest fix: extend that comment with the two keys and "(ADR-0040)". Same for `docs/adr/0019-…md:63`, which is where the payload is actually pinned and which carries no amendment note — the repo's convention is to annotate in place (`docs/adr/0018-…md:46-52` does it three times).

### Finding: P2 — "logged and skipped" is satisfied only at `debug`, and the default filter is `info` (Spec)

`rpc.rs:315-325` logs the per-turn declined count with `tracing::debug!`; `logging.rs:13` sets `DEFAULT_FILTER = "info"`. So under default settings an unknown or malformed frame is skipped and produces **no** log line at all, which is not what acceptance bullet 4 says. The `Lagged` arm, by contrast, is `warn`. ADR-0040:78-81 quietly restates the acceptance as "one `debug` line per turn".

Smallest fix: promote the per-turn count to `warn` when `skipped > 0` (it is at most one line per turn, like the lag warning), or say in ADR-0040 why `debug` is what "logged" means here. No test constrains either choice.

### Finding: P3 — a test that cannot be the first to fail

`tests/rpc.rs:182-197` (`a_fixture_whose_frames_drifted_fails_this_guard`) asserts `settled.len() == 1`, one `message.end`, one `turn.start` and non-empty text for both fixtures — all of which `the_recorded_text_turn_becomes_the_contracts_frames_in_order` and `the_recorded_thinking_turn_streams_thinking_before_text` already assert, more strictly. No mutation to the mapping can make this test red while those two stay green, so its doc ("the guard that makes the two replays above meaningful") is inverted. `docs/build.md:339` then leans on it. Fix: assert something the replays do not — e.g. the exact record-type multiset per fixture, or a line count — or drop it and point the doc at the replays.

### Finding: P3 — `a_closed_agent_reports_a_closed_stream` covers neither arm it names

`tests/rpc.rs:521-538`: the comment says "Nothing is launched" and then calls `Agent::launch`; the assertion cannot distinguish `Ok(None)` (the reader's last word, the production arm) from `Err(Closed)` (the channel closing, unreachable while the registry holds the `Agent`), and the `done` cache (`agent.rs:270-282`) is never exercised. Fix: run a script that prints one record and exits, assert `Record` then `Closed`, then a second `Closed`.

### Finding: P3 — duplicated test scaffolding and one assertion that does not match its comment

- `tests/rpc.rs:200-241` builds `Log` (database + session); `an_agent_that_dies_mid_turn…` (`:395`) and `a_turn_that_settles_aborted…` (`:472`) each rebuild the same database/session inline (~20 lines, twice). Use `Log::new`.
- `backend/docker.rs:1083-1084`: the comment says "Across operations too, which is the case that matters: a steer and an abort racing", but the assertion compares two `command_id("abort")` values. Compare `command_id("steer")` with `command_id("abort")`.

### Finding: P3 — "the process's start instant" is the instant of the first mint

`rpc.rs:49-56` (`minted_instant`), ADR-0040:28-33 and `GLOSSARY.md`'s **Message id** entry all say the id carries *the process's start instant*; the `OnceLock` is filled lazily on the first `mint()`, which can be hours later. Uniqueness is unaffected; three sentences are. Fix the wording (or initialise eagerly if the wording matters).

### Finding: P3 — smaller doc/behaviour mismatches with file:line

- `rpc.rs:161-166`: `"start" | "done" | "error"` return `Mapped::Nothing` **without counting or logging**. For `error` the stated reason ("the content is already on screen as deltas") is false — a provider error event carries a message — so an errored stream leaves no diagnostic anywhere but pi's `stopReason`. Count it, or log it.
- `docker.rs:149-154`: `supersede_agent` "drops the receiver nobody is going to take" — `supervise` takes it in the same provisioner run right after `Step::Pi` (`provisioner.rs:114-115`, `docker.rs:791-797`), so in every path that matters the field is already `None`; the generation bump is the whole mechanism. Trim the sentence.
- `docker.rs:262-269`: the operator sentence "the clear_queue: this session has no agent process — resume it" leaks an underscore into prose (asserted verbatim at `docker.rs:1055-1061`). Pass a human label separately from the operation key.
- `docs/adr/0040-…md:55`: "ADR-0018 pinned `message.end` as …" — `message.end` is not in ADR-0018's table at all; it is pinned in `docs/adr/0019-…md:63`.
- `docs/build.md:335-340`: "Neither **the gate** nor any test needs a provider key, a daemon or the network" — `case_docker_turn` *is* a gate case and skips without a daemon and `pi-agent:local` (`scripts/verify.sh:776-790`). Say "neither `tests/rpc.rs` nor any replaying test".
- `docs/adr/0039-…md:160-163` hands this story two follow-ups it does not pick up: "HS-032 owns the revisit" of `--no-session` (a resumed session's pi has no memory of the earlier turn) and "stdout's non-`response` records are counted and dropped", now false. ADR-0040 should annotate 0039 and record the `--no-session` consequence, as the repo's convention does for superseded ADRs.

---

### Verified by reading vs. not verified without running

Verified from source: the mapping's arms and their unit tests, the pump's three arms, the reader's publish/route split, `start_turn`'s ordering, the four operations' timeouts, the exit-classification path and its generation compare, the resume path through the provisioner, the client's handling of `turn.*` and `message.end`, pi's record semantics (its own `docs/json.md`), the fixture contents, the gate case's assertions and its config, and the doc/ADR claims quoted above.

Not verified without running:

- **`cargo test` / `cargo clippy` / `cargo fmt --check` / `scripts/verify.sh`.** Please run `scripts/verify.sh` (it runs fmt, clippy, the web tests, `cargo test` and the serve cases) and, separately, `cargo test --test rpc` and `cargo test --lib rpc`.
- **`tests/agent.rs::the_turns_commands_reach_pi_and_a_refusal_is_pis_sentence`** needs a Docker daemon and `pi-agent:local` (`HOLOSMITH_TEST_IMAGE`).
- **The P1 flake.** Reproducing it needs Docker: run `case_docker_turn` (or a resume-then-prompt loop) repeatedly and watch for `"status":"error"` with "the container exited 137" right after `POST /resume`. With the fix as written I expect it to recur at roughly the rate ADR-0040 reports.
- **Whether pi emits two `turn_start`/`turn_end` pairs for one prompt on retry or tool use** — pi's own doc for the newer release says so and the plan asserts the pinned release's doc is identical, but only a captured tool-using fixture would settle it.
- **Whether 1024 records can actually be outrun** by a real provider through the writer's bounded queue at `synchronous=FULL`; the loss path is real in code, its frequency is a measurement.

**Merge verdict: BLOCK** — one P1 (the removal-supersedes fix is not on the resume path, and three documents say it is), plus P2s on the turn-boundary semantics, the lossy channel against the acceptance line, and three stale/incorrect contract documents.
## Resolution

| # | Finding | Verdict | What changed |
| --- | --- | --- | --- |
| P1 | The "removal supersedes the launch" fix is not on the resume path: `start_container` called `Docker::remove_container` inline, so `DockerBackend::remove_container`'s `supersede_agent` never ran on a resume, while three documents said the flake was fixed | **Fixed** | `start_container` now calls `self.remove_container(session)` — one removal, one place to forget the launch (`docker.rs`). Re-verified: the demo's resume-then-prompt ran twice more and answered `error`/`idle` both times; `case_docker_turn` asserts the same shape in the gate. This was the reviewer's best catch of the session and the reason a fix needs its own proof, not a doc. |
| P2 | pi's `turn_start`/`turn_end` are per *assistant response*; the transcript's `turn.*` is per *prompt* | **Fixed** | The mapping **declines** pi's boundaries and `rpc::pump` writes `turn.start` before the first frame and `turn.end` on the settle. ADR-0040 has a section on it, `rpc.rs`'s module doc explains it, and the unit test that asserted the old mapping is now `pis_own_turn_boundaries_are_declined_and_the_settle_ends_the_turn`. A turn whose agent never settles writes no `turn.end` (asserted in the dies-mid-turn test). |
| P2 | The record channel is lossy (`broadcast` drops the oldest), against "every delta reaches the writer" | **Fixed** | Replaced with a bounded `mpsc` behind `agent::Slots`: the reader **awaits** the send, so a slow commit slows pi instead of dropping text. `Received::Lagged` is gone; `Closed` is the reader's own `Slots::close`, which is what makes it about the process rather than the registry entry. ADR-0040 records why the first draft was wrong. |
| P2 | `read_records`'s doc still said non-`response` records are "counted and skipped", and the module doc never mentions the new seam | **Fixed** | Both rewritten: the reader publishes, `response` is routed, only unparsable lines and orphan responses move the counter; the module doc gained a bullet on the record seam. |
| P2 | `message.end`'s wire shape lives in `events/mod.rs`'s doc comment and ADR-0019's table, and neither was updated | **Fixed** | Both carry `stop_reason` and `tool_calls` now, and ADR-0019 has an "Amended by ADR-0040" line (the convention ADR-0018 already uses). |
| P2 | "Logged and skipped" was a `debug`, which the default `info` filter hides | **Fixed** | Two counters: **declined** (a documented record with no frame — the design, one `debug` per turn) and **unknown** (a type or shape this build has never seen — one `warn` per turn, which is how a pi release that grew a frame gets noticed). pi's nested `error` event also warns with its own `reason`/`error`, since that is the only place the sentence exists. |
| P3 | `a_fixture_whose_frames_drifted_fails_this_guard` asserted only what the two replays already assert | **Fixed** | It now counts the **record types per fixture** — the one thing a mapping assertion cannot see, because a frame cannot be missing for a record that is not there. Renamed `…_records_drifted…`, and `docs/build.md` was corrected with it. |
| P3 | `a_closed_agent_reports_a_closed_stream` covered neither arm it named | **Fixed** | Rewritten as `the_records_end_with_the_process_and_answer_closed_from_then_on`: a script prints one record and exits, and the test asserts `Record` → `Closed` → `Closed` (the last is the `done` cache the old test never reached). |
| P3 | Duplicated test scaffolding; the `command_id` test's "across operations" assertion compared two `abort`s | **Fixed** | Both tests reuse `Log::new`; the assertion compares `steer` with `abort`. |
| P3 | "The process's start instant" is really the first mint's instant | **Fixed** | `rpc.rs`, ADR-0040 and `GLOSSARY.md` say the instant is taken on the first mint, and why uniqueness does not care. |
| P3 | `rpc.rs`'s `start`/`done`/`error` arm returns `Nothing` without counting, and the stated reason is false for `error` | **Fixed** | `start`/`done` are declined (counted); `error` warns with pi's reason and is declined. `toolcall_*` is declined on purpose (HS-033's). |
| P3 | `supersede_agent`'s doc claims the receiver is dropped as if that were the mechanism | **Fixed** | The doc says the generation bump is the mechanism and the receiver drop is belt and braces (`supervise` takes it in the same provisioner run). |
| P3 | The no-agent sentence leaks `clear_queue` into prose | **Fixed** | `live` takes the sentence's subject ("the turn", "the steer", "clearing the queue", "the abort"); the unit test asserts each. |
| P3 | ADR-0040 cites ADR-0018 for `message.end`, which is pinned in ADR-0019 | **Fixed** | The citation is ADR-0019 and the amendment is recorded there. |
| P3 | `docs/build.md` says "neither the gate nor any test" needs a daemon, which is wrong for `case_docker_turn` | **Fixed** | It says no test that *replays a fixture* needs one, and that the gate case skips without a daemon and an image like every other Docker case. |
| P3 | ADR-0039's two handoffs to this story (the `--no-session` revisit, and "non-`response` records are counted and dropped") | **Fixed** | ADR-0039 now annotates both: the record seam is superseded by ADR-0040, and ADR-0040 records the answer to the `--no-session` question — a resumed agent has no memory, and replaying the log into it is a story of its own, named as a follow-up rather than left to be discovered. |
| — | The artifact was one commit behind the tree while the reviewer read | **Acknowledged** | Regenerated mid-review; the reviewer read the tree as well, which is why it found the tests and the ADR rewording the artifact lacked. Not a finding against the code. |

Everything else the review verified by reading — the mapping's arms, the subscribe-before-write ordering, the
fixture's contents, the four operations' timeouts, the phase rule (no API or UI change), and that no session's
frames can reach another's log — needed no change. The gate after the fixes: `scripts/verify.sh`, 51 cases and
434 assertions, green.
