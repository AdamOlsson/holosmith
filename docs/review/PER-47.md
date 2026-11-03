# Review — PER-47 (HS-015, prompt and the turn lifecycle)

Fixed point: `527931a`. Diff: `527931a...HEAD` plus the three new files. Reviewer ran
read-only with no shell; its correctness claims were checked against the transitioner, the
provisioner, the transcript model and the verify harness by reading them.

## Review

**Correct (verified, not taken on trust)**

- **The decision order is right and each answer tents against the one above it.** `http/sessions.rs:386-419` decides body → row → state → transition; `ApiError::not_idle` derives its code from the state's own machine word (`http/error.rs:248-258`) and the test proves the constants against `Status::as_str` rather than restating them (`http/error.rs:440-470`). A client cannot get a 202 for a session that is not `running`: the row is refetched after the transition and the runner is spawned only then (`sessions.rs:412-419`).
- **No lock across an `await`, and the compiler is the proof.** `Turn::touch` (`turn.rs:99-108`) takes `Arc<Mutex<Database>>` in a synchronous scope; `Turn::run` is `tokio::spawn`ed (`turn.rs:63-65`), so a guard held across an await would not compile. Same rule the provisioner documents.
- **The backend cannot move a state — the row, at least.** `SessionBackend::start_turn` gets `&EventWriter` only (`backend/mod.rs:155-166`); `Turn` is the only holder of `Arc<Mutex<Database>>` (`turn.rs:41-45`), so no backend can build a `Transitioner`.
- **The turn cannot deadlock or double-run.** `Transitioner::apply` reads and writes inside one lock scope (`state.rs:370-394`), so a lost race is decided atomically; the loser retries and can only start a turn after the winner's `start_turn` returned and its settle wrote `idle` (`turn.rs:76-83`). No interleaving produces two live turns on one session.
- **ADR-0018's core rules survive the new kinds.** The usage line is an appended `usage` op inside the message block (`main.ts:520-523`), tools get a separate keyed map and order (`main.ts:487-497`, `transcript.ts:194-209`), a delta is still one appended text node, and the three new kinds are in `TRANSCRIPT_EVENTS` (`transcript.ts:53-61`) so they are parsed, not dropped.
- **The composer cannot double-submit, wipe the transcript, or inject markup.** `prompt.disabled = true` is the first statement of `send` (`main.ts:572`), the submit handler gates on `canPrompt && !prompt.disabled` (`:604`), `preventDefault` is unconditional (`:600-606`), and every server string reaches the DOM as `textContent` (`make` `:130`, `say` `:555-558`, badge `:707-709`).
- **The gate cases are sound.** `serve_cases` is 26 entries, 3 new; `case_prompt_turn` re-reads the stream from seq 1 (replay, `REPLAY_LIMIT = u32::MAX`) so it cannot race the 202, and 19 frames ≈ 76 body lines fit `fetch_sse_until`'s 200-line/3 s bound.

---

### Standards axis

**S1 · P2 · `backend/stub.rs:262-266` — the comment promises a behaviour the code reverses.**
> `// … the loss is logged and the turn carries on, as the provisioner's step reports do.`
> `if let Err(error) = events.append(event).await { … return Err(BackendError::failed(error.to_string())); }` (`:273`)

The turn does not carry on. Because `Turn::run`'s `Err` arm applies `unexpected_exit` (`turn.rs:83-92`), a *successful* turn is written `error` with reason "the event writer has stopped…" purely because the writer died during shutdown — a terminal state item 8's recovery row (`starting|idle|running → starting`) does not cover, so only a manual `resume` clears it. *Smallest fix:* either `continue` as the comment says, or drop the clause and record the shutdown consequence in ADR-0019.

**S2 · P2 · `backend/stub.rs:37-39` — the count is wrong.**
> `/// The six kinds a turn's script emits … Four are ADR-0018's; tool.start, tool.end and message.end are HS-015's`

3 + 3 = 6; "four" would make seven, and `message.snapshot` is not emitted at all (`script`, `:177-226`). *Fix:* "Three are ADR-0018's".

**S3 · P2 · ADR-0018 is contradicted by this diff and never annotated.** ADR-0018:10 ("The four event kinds, pinned before their producer exists"), :47 ("A frame whose kind this screen does not know — `tool.start`, `usage`, later kinds — is dropped, so HS-032 through HS-034 add kinds without a page release"), :86-87 ("pressing one names the story that will wire it"), and `docs/stories/README.md:136` ("Consequence: HS-015 must emit exactly these kinds"). This diff has HS-015 emit three more kinds *and* the page render them. The repo's established remedy is a one-line note in the older ADR — `docs/adr/0012-…md:27` and `:74` ("**Superseded by ADR-0013:** …"), ruled on in `docs/review/PER-41.md:37`/`:75`. *Fix:* two "superseded by ADR-0019" lines in ADR-0018 (:47, :86-87) and adjust README:136.

**S4 · P2 · `docs/adr/0019-…md:41` overstates the enforcement of the split.** "…the source-level guard of ADR-0016 … so 'the backend decides when a session is idle' is unexpressible rather than merely discouraged." The guard matches the literal token `set_status` (`state.rs:644-669`). A backend now holds a writer whose `append` takes a free-form `NewEvent { kind: String, payload: String }` (`events/mod.rs:56-63`, `:82-88`), and the stream re-frames whatever `kind` a row carries (`events/stream.rs:84-87`). A backend can emit `session.status {"from":"running","to":"idle"}` and every client shows `idle` while the row says `running` — the exact lie "row first, event follows" forbids. *Fix:* one sentence ("it cannot write the row; a forged report is what ADR-0016's rule forbids"), or type the payload so a status frame cannot be built outside `state.rs`.

**S5 · P2 · an expected race is logged at ERROR.** `state.rs:375-382` logs `tracing::error!("refused an illegal state change")` on every `Illegal`, and this diff makes `Illegal` the *expected* outcome of a legal request (`sessions.rs:429-435` retries it). Two tabs double-tapping produce an error-level line that reads like a bug. *Fix:* demote that line to `debug!`; the call sites that treat `Illegal` as impossible (`turn.rs:118-125`, `provisioner.rs:152-158`) already log at `error`.

**S6 · P2 (smell: Duplicated Code) · `turn.rs:110-140` duplicates `provisioner.rs:145-170`,** and `AppState::turner()` (`http/app.rs:73-83`) duplicates `AppState::provisioner()` (`:57-70`). Only the tracing target and message text differ; HS-016's abort ladder makes a third copy. *Fix:* one reporter in `sessions::state` that both runners call.

**S7 · nit · `main.ts:432-439`** — `toolBlock` leaves `.tool-output` visible-but-empty until a `tool.end` arrives (only `:507` sets `hidden`), so a mid-call block carries `style.css`'s `margin-top` under nothing. Same class as `docs/review/PER-46.md:88`. *Fix:* `output.hidden = true;` in `toolBlock`.
**S8 · nit · `docs/adr/0019-…md:53`** says "Two payloads are pinned here" over a three-row table, and `:93` says "the four `bad_body` shapes" while `tests/turns.rs` iterates five.

**Standards verdict: OK with notes.** No documented-standard breach that breaks a gate; S1/S2 are code-comment errors inside the diff, S3/S4 are doc claims the code does not keep, S5-S8 are hygiene.

---

### Spec axis

**P1 · P1 · the stub reuses message and tool ids after a restart, corrupting one session's transcript.** `backend/stub.rs:51` `static NEXT_TURN: AtomicU64 = AtomicU64::new(1);` and `:178-179` `format!("msg-{turn}")` / `format!("tool-{turn}")`, justified at ADR-0019:48-49: "ADR-0018 asks only that a message id be unique within a session, and one counter in one process gives that". Events are durable (ADR-0013) and a client folds the whole replay on load, holding one block per id and never a second (`transcript.ts:213-216`; `main.ts:506-507` assigns, it does not append). So `serve --backend stub`, prompt, restart, prompt again ⇒ turn 2's text appends into turn 1's block and turn 2's `tool-1` end overwrites turn 1's output. That breaks ADR-0018's "one block per message id" and acceptance bullet 4's "visible boundaries", silently and permanently in the database. *Smallest fix:* mint the id from something durable — seed the counter from the session's `last_seq`, or spell `msg-<unix_now>-<n>`.

**P2 · P2 · the handler can leave a row in `running` with no runner and no way out.** `sessions.rs:429-435` maps `StateError::Unreported` to `internal()` — but that variant's own doc says "The state changed and the event that reports it did not reach the writer … The row is already right, so this is a warning, not a rollback" (`state.rs:270-272`, returned at `:410`). Likewise `sessions.rs:412-418` answers 404/500 from the post-transition refetch *before* `state.turner().spawn(…)` at `:418`. In both, the row is `running`, nothing settles it (abort is HS-016, timeout HS-022, and `Trigger::CrashRecovery` has no production caller), and the client is told the prompt failed. This is focus (b)'s stranding on the failure paths. *Fix:* treat `Unreported` as success, and spawn from the row the transitioner already returned.

**P2 (report-only) · the "two races in a row" arm answers 500 for a session that is `idle`.** `sessions.rs:443-447`. It needs two non-idle windows between two atomic applies, so it is effectively unreachable, and the comment reasons about it honestly; but the client gets a code it cannot act on and a third apply would probably win. *Fix:* retry, or answer the fresh state's code.

**Acceptance, bullet by bullet**
1. *202, `idle → running`, 409 unless `idle`* — satisfied (`sessions.rs:386-419`; five per-state codes, `error.rs:236-259`; `tests/turns.rs` socket cases). The body-first 422 is a documented order (ADR-0019:73-77) and is tested before the row read.
2. *`agent_settled` the only normal way out* — satisfied (`turn.rs:76-83`; table row `running + agent_settled → idle`, `state.rs:196-200`).
3. *script has thinking, text, a tool call, usage* — satisfied (`stub.rs:177-226`: 3 thinking, 4 text, `tool.start`/`tool.end`, `message.end` with `{"in":1200,"out":340}`).
4. *explicit turn start/end* — satisfied in shape (`turn.start`/`turn.end`), **weakened by the P1** above across a restart.
5. *`last_active_at` on prompt, settle, steer* — prompt and settle satisfied (`turn.rs:70`, `:79`; `repository.rs:184-199`, whose doc records that `set_status` deliberately leaves the column alone); steer is HS-017's, as stated. One note: a turn ending in `error` does not touch (`turn.rs:83-92`) — not a "settle", so report-only.

**Not verifiable here, stated rather than hidden:** the DOM half of the screen (no browser), and the claim of "98 assertions" (I did not run the harness). The supervisor must run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (`tests/turns.rs` is 9 socket tests), `cd web && npm test`, and `scripts/verify.sh`.

**Spec verdict: OK with notes.** All five bullets are implemented, none by prose alone, and the deferred items (abort/steer, `output_ref`, usage persistence, paging/coalescing) match the brief's stated limitations. The P1 is stub-only and a one-line fix, but it is a real break of bullet 4 for the demo backend.

---

**Merge verdict: OK with notes.** 1 × P1 (stub ids reset per process), 5 × P2, 4 × nit; nothing blocks the merge on correctness of the prompt/turn lifecycle itself, and the P1 should land in this change.
## Resolution

Every finding triaged. Fixed means the change is in this commit.

| Finding | Verdict | How |
| --- | --- | --- |
| P1 · the stub mints `msg-<n>`/`tool-<n>` from a counter that resets per process, so a restart reuses ids and one session's blocks merge | **fixed** | The id now carries the **process instant**: `msg-<nanos-since-epoch>-<turn>` and `tool-<…>-<turn>`, read once per process from a `OnceLock`. The log is durable (ADR-0013) and a client folds the whole replay into one block per id, so the id's range has to outlive the process; the stub has no database handle by design, so the process's own start instant is the durable-ish source. Pinned by `backend::stub::tests::a_turns_ids_are_not_just_the_counter`, which fails on the old `msg-<n>` shape. |
| S1 · the stub's comment said the turn carries on while the code failed the turn | **fixed** | The code follows the comment and the provisioner's precedent: a dead writer (which in a served process means the process is leaving) is logged and the turn completes, because failing a turn that ran would write a finished session into `error` on shutdown. |
| S2 · "The six kinds … Four are ADR-0018's" | **fixed** | "Three are ADR-0018's (`turn.start`, `message.delta`, `turn.end`)", and the comment now says `message.snapshot` is not emitted. |
| S3 · ADR-0018 is contradicted by this diff and never annotated | **fixed** | Two **Superseded by ADR-0019** lines in ADR-0018 (the drop rule, and the composer's unwired buttons), and `docs/stories/README.md`'s ADR-0018 bullet now says HS-015 adds three kinds. |
| S4 · ADR-0019 overstates the enforcement of "the backend cannot move a state" | **fixed** | The ADR now says what is true and what is not: the row is untouchable from behind the trait, but a backend holds a writer whose kind and payload are free-form, so it *could* forge a `session.status` frame — the lie ADR-0016 forbids. Closing that means typing the payload (no story asks for it yet), and the hole is recorded rather than denied. |
| S5 · every refused transition logs at `error`, and a lost prompt race is now expected | **fixed** | Demoted to `debug`, with the reason: the callers that treat `Illegal` as impossible log it themselves (`apply_logged` at `error`). |
| S6 · the turn runner's and provisioner's `transition` helpers, and two `AppState` wiring helpers, are duplicated | **fixed** | One `Transitioner::apply_logged` in `sessions::state` owns the "log, never return" convention; both runners are now one line each. `AppState::turner()` still mirrors `provisioner()` — that is four lines of struct plumbing, and folding them would be a builder for two callers. |
| S7 · `toolBlock` leaves `.tool-output` visible-but-empty mid-call | **dismissed** | It uses the shared `kindNode` helper, which sets `hidden = true` on creation (`main.ts:432-439`, `kindNode` at `:406-411`), so a mid-call block shows only its name; `tool.end`'s append unhides it. The reviewer read the helper as `make`. |
| S8 · ADR-0019 says "two payloads" over three rows, and "four `bad_body` shapes" while the tests iterate five | **fixed** | Both counts corrected. |
| P2 (report-only) · the "two races in a row" arm answers 500 for a session that is idle again | **dismissed** | It needs two non-idle windows between two atomic applies, which the reviewer itself calls effectively unreachable. The honest alternative it suggests — a state code — would have to name a state the row is no longer in; the fixed 500 sentence is at least true. |
| P2 · `Unreported` is mapped to 500, and a refetch failure answers before the runner is spawned, both leaving a row in `running` with nothing to settle it | **fixed** | `Unreported` is now the prompt **succeeding** (the row is right; only the event was lost to a dying writer), and the turn is spawned **before** the 202's body is refetched: a refetch that fails now falls back to the row the handler already read, with the status the table wrote, instead of stranding a running session. |
| Own finding (from the demo) · the prompt's 422 reused the create's sentence ("…`project`, `model`, `thinking`…") | **fixed** | `ApiError::bad_prompt()`: the same `bad_body` code — the code is the API, the sentence is prose (ADR-0008) — with a prompt's own sentence, and a blank-after-trim prompt is refused too. |
| Own finding · `ApiError::not_idle(Idle)` was `unreachable!()`, reachable when a competing turn settles between the read and the refusal, and a panic in a handler drops the connection with no envelope | **fixed** | `apply_prompt` retries once when the row reads `idle` again (the prompt is legal then), and `not_idle` is total: `Idle` answers the fixed 500 sentence rather than panicking. |
| Worker's own report · `cargo test` total was given as 437 | **corrected** | The real figure is **409 tests: 207 lib + 202 across 16 integration binaries**. |

Gates re-run after the fixes, alone on a clean tree: `cargo fmt --check`, `cargo clippy --all-targets --
-D warnings`, `cargo test` (409), `cd web && npm test` (15), `scripts/verify.sh` (26 cases, 98 assertions).
