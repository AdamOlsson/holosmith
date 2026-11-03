# Review — PER-49 (HS-017, steer a running turn)

- **Fixed point:** `3d2ecba`
- **Reviewed change:** `0099ca3` (`3d2ecba..HEAD`)
- **Reviewer:** a fresh `reviewer` subagent running `/home/adamolsson/.agents/skills/code-review/SKILL.md`,
  read-only, no shell. Its claims were checked by the orchestrator — including the one it proved by
  mutation, below — and the Resolution table records what was fixed and what was dismissed.
- **Verdicts:** Standards — not clean (2 should-fix, both doc corrections); Spec — not clean (1
  should-fix: an acceptance bullet claimed but not proved).

This review is saved verbatim. The `## Resolution` section at the end was appended afterwards.

## Review

Read: the brief, `code-review` skill, the full diff (`3d2ecba..HEAD`), and the touched files in the tree, plus `HolosmithSpec.md` item 8 + the API table, `GLOSSARY.md`, `docs/build.md`, ADR-0018/0019/0020/0021, `docs/stories/…` HS-017, `scripts/verify.sh`, `web/src/{main,transcript}.ts`, `style.css`, and the prior review artifacts in `docs/review/` (PER-46/47/48) for convention. No files modified.

### Correct (both axes share these)
- Route seam is the established one, not a second one: `Routes::post` ⇒ `RouteKind::Mutating` (`crates/holosmith/src/http/app.rs:167-178`), both extractors taken as `Result` with `unwrap_or_default()` (`app.rs:423-432`), exactly as prompt/abort — so a `%FF` segment stays inside ADR-0008's envelope.
- Lock discipline holds. `one_locked` returns an owned `Session` (`sessions.rs:388-398`), `touch` is synchronous (`sessions.rs:543-556`), and the guard is not `Send`, so the module's own rule (`sessions.rs:12-14`) is compiler-enforced. No guard lives across the `state.backend.steer(…).await` (`sessions.rs:522`).
- Broadcast-after-COMMIT is preserved by construction: the handler only queues (`events/mod.rs:82-87`); the writer thread commits then broadcasts (`events/writer.rs:82-90`).
- No state write outside `Transitioner`: `touch` writes the activity column, not `status`, so `no_status_write_outside_the_one_function` (`sessions/state.rs:700-727`, non-vacuity assertion intact) still holds over the new code.
- `bad_steer` follows ADR-0008's "code is the API, sentence is prose" exactly as `bad_prompt` does (`http/error.rs:145-171`).
- Cross-references mostly updated: `docs/stories/README.md:117` Twenty → Twenty-one with ADR-0021 listed; `docs/build.md:137-141`; and ADR-0021's "30 cases" is correct — I counted 30 entries in `serve_cases` (`scripts/verify.sh:255-290`).

---

## Standards

**S1 · should-fix (P1) — ADR-0018's supersession chain was not extended, so a canonical doc is now false.**
`docs/adr/0018-…-one-block-per-message.md:91`: *"**Superseded by ADR-0020:** abort is wired too; only steer still names its story."* The diff wires steer (`web/src/main.ts:572-603`) and adds ADR-0021 but appends no note. The repo's convention is explicit and was *fixed*, not dismissed, one story ago — `docs/review/PER-48.md:31,70` raised this exact nit ("*Smallest fix:* one `**Superseded by ADR-0020:** abort is wired too") and item 6 records it as **Fixed**. Same class, second instance: `docs/adr/0020-…:130` still says *"`steer` and `resume` still have no caller"*.
*Smallest fix:* one sentence in ADR-0018 — "**Superseded by ADR-0021:** steer is wired too; no composer action names a later story."

**S2 · should-fix — a stale comment survives in the very function the diff rewrote.**
`web/src/main.ts:537-541` (the `Composer` doc, unchanged by the diff): *"Prompting posts for real (HS-015) and aborting does too (HS-016); steering keeps the notice naming the story that wires it, because a request that would 404 is worse than a note."* That is now the opposite of the code 30 lines below it (`main.ts:572-575`, "Steering is a real request (HS-017)"). It is the block header a reader hits first.
*Smallest fix:* replace the last clause with "steering posts for real too (HS-017)."

**S3 · consider (P2) — the new block classes have no style rules, and one loses line breaks.**
`web/src/main.ts:497-498` creates `article.steer`, `div.steer-label`, `div.steer-text`; `web/src/style.css` has a rule for every sibling block class (`.message:191`, `.thinking:201`, `.text:206`, `.tool:212`, `.tool-name:218`, `.tool-output:224`, `.usage:233`, `.turn-boundary:239`) and none for these three. Beyond looks, `.steer-text` has no `white-space: pre-wrap` (compare `.text:206-208`), and the composer's box is a `textarea` (`main.ts:553-556`, `rows = 3`), so a two-line steer renders as one line. ADR-0021:61-62 claims the block is "drawn as `article.steer` with the operator's text" — true, but the operator's line breaks are not drawn.
*Smallest fix:* a `.steer`/`.steer-label`/`.steer-text` rule in `style.css` with `white-space: pre-wrap` on the text.

**S4 · nit (Duplicated Code, judgement call) — two `touch` helpers, near-verbatim.**
`sessions.rs:543-556` duplicates `turn.rs:141-150`: same lock, same `sessions().touch(id, clock::unix_now())`, same "log it and carry on" warn; only the message differs. *Smallest fix:* one `fn touch(database: &Mutex<Database>, id: &str)` in `sessions/repository.rs`'s neighbourhood, called from both.

**S5 · nit (Duplicated Code) — `parseFrame`'s steer branch re-implements the guard below it.**
`web/src/transcript.ts:114-126` repeats `JSON.parse` + "object, not null, not array" from `:138-152`. *Smallest fix:* hoist the parse and object-check above the kind dispatch, then branch; also name the payload `payload` (the file's own word at `:138`) rather than `steer`.

**S6 · nit — the diff uses the word the glossary tells it to avoid.**
`GLOSSARY.md:110` lists *inject* under **Steer**'s `_Avoid_`, and ADR-0021:13-14 makes that the argument for the kind's name — yet three new comments say "injected": `sessions.rs:558` ("show what was injected and when"), `main.ts:495`, `verify.sh:861`. They are quoting the Linear bullet, which is defensible, but the repo's own vocabulary rule is being quoted against itself. *Smallest fix:* "what was delivered" (the ADR's own word).

**S7 · nit — the same failure is logged twice.**
`sessions.rs:526-531` warns "a steer was not delivered", then `internal("steer", error)` (`sessions.rs:532`) logs `tracing::error!` with the same error (`http/error.rs:285-293`). `prompt`'s path logs once. *Smallest fix:* drop the `warn!` and let `internal` carry it.

**S8 · nit — ADR-0021 names a symbol that does not exist.**
`docs/adr/0021-…:85`: *"The `register_steer` path holds no lock across an `.await`"*. Nothing in the tree is called `register_steer` (the handler is `sessions::steer`; registration is inline in `router`). *Smallest fix:* "The steer handler".

**S9 · nit — two docs describe a check the script does not make.**
`docs/adr/0021-…:81-82` and `docs/build.md:137-139` say `case_steer` "reads it again for the settle that proves the turn continued". `scripts/verify.sh:911` calls `wait_for_idle`, which polls `GET /api/sessions/$created_id` (`:930-945`) — a row poll, not a stream read. The script's own comment (`:906-909`) is the accurate one. *Smallest fix:* "then polls the row until it says `idle`".

**S10 · nit — `wait_for_idle` breaks the documented failure contract.**
`verify.sh:933-934`: `fetch "$label" GET … || return 1` returns 1 with no `FAIL` line, whereas every case converts a failed fetch (`case_steer:868-870`, `:875-877`). Under `set -e` a failed poll exits silently, and `docs/build.md:158-159` promises "a failure prints `FAIL <assertion>`". *Smallest fix:* `|| fail "$label/no_answer" "GET /api/sessions/$created_id did not answer"`.

**S11 · nit — two small doc drifts inside `http/error.rs`.**
`:161` says "The same code again" where `bad_prompt:148-149` links `[Self::bad_body]`; and `:255` still reads "…HS-016 adds `session_idle` … and HS-017 **will** name it too", which this story now does.

**Standards verdict: not clean — 2 should-fix (both one-line doc corrections), 2 consider, 7 nit.** Nothing here is a code-correctness defect; S1/S2 are the two a later reader would be actively misled by, and S1 has a fixed precedent from the previous story.

---

## Spec

Against `docs/stories/phase-1-session-lifecycle-stub.md:190-192` and Linear PER-49:

- Bullet 1 (*202 while `running`*) — implemented (`sessions.rs:505-537`), proved (`tests/steer.rs:158-217`: 202, `content-type`, body `status == "running"`), and in the shipped binary (`verify.sh:863-911`). **Correct.**
- Bullet 2 (*`409 session_stopped` on stopped, `409 session_idle` on idle, UI offers a prompt*) — implemented (`sessions.rs:519-521` via the total `not_in_state`, `error.rs:262-273`), proved (`tests/steer.rs:221-259`, driven through the table's `idle_stop` row because HS-018 does not exist), and in the UI (`main.ts:588-594` keeps the text and names the action; `affordances("idle")` puts the Prompt button on screen). **Correct.**
- Bullet 3 (*recorded as an event*) — `turn.steer` with `{"text": …}` (`sessions.rs:50-56`, `:563-579`), parsed and rendered as one block per frame (`transcript.ts:114-126`, `:214-218`; `main.ts:493-500`). **Correct.**
- Demo (*steer while running, guidance in the transcript, turn continues*) — `tests/steer.rs:214-216` waits for `idle` and asserts `status_reason: None`; `verify.sh:911`. **Correct.**

**SP1 · should-fix (P1) — the `last_active_at` assertion cannot fail on the steer's behalf; it is satisfied by the settle.**
`tests/steer.rs:172` captures `let before = row(&state, &id).last_active_at;`, then `:208-212`:
```rust
// HS-015's acceptance: `last_active_at` moves on prompt, settle and steer.
wait_until("last_active_at moved", || {
    row(&state, &id).last_active_at > before
})
```
`clock::unix_now()` is **seconds** (`crates/holosmith/src/clock.rs:10-17`), so the steer's own `touch` — milliseconds after `before` — writes the same second and can never satisfy `>`. What satisfies it is the *settle's* touch (`turn.rs:85`), ~1.2 s later on `paced(100)` (12 frames × 100 ms), which the same test then explicitly waits for at `:215`. Delete `touch(state, &current.id);` from `sessions::steer` (`sessions.rs:534`) and this test stays green. HS-015's bullet 4 ("`last_active_at` updates on prompt, on settle and on steer", `phase-1-session-lifecycle-stub.md:161`) is therefore **unproven for steer**, and ADR-0021:74's "`last_active_at` moved" is an overclaim. `docs/review/PER-47.md:62` recorded this bullet as "steer is HS-017's, as stated" — this is the story that was supposed to close it.
*Smallest fix:* stop racing the second. Either (a) push the row's activity into the past through the repository the test already holds (`state.database` → `sessions().touch(id, now - 60)`), then steer and assert the value moved to `clock::unix_now()`; or (b) `paced(1_500)` and assert the move *before* `wait_idle`, with a comment saying the settle cannot have happened yet. (a) is deterministic and needs no new timing.

**SP2 · consider (P2) — the 409 refusals do not prove nothing reached the agent.**
`tests/steer.rs:221-259` asserts only status + `content-type` + `code` via `assert_envelope`. The 422 test *does* check the backend (`:287-293`, `assert!(steers(&state).is_empty())`). A handler that called `backend.steer` before the state check would keep this test green. *Smallest fix:* one `assert!(steers(&idle_state).is_empty(), …)` after the two 409s (`steers` already reads `state.backend`, `:296-304`).

**SP3 · consider (P2) — a test name and an ADR claim ordering that the test cannot observe.**
`tests/steer.rs:263` is `a_steer_body_of_the_wrong_shape_is_bad_body_before_anything_else`, but it runs against a **running** session (`:266-268`), so it proves body-before-*agent* only. ADR-0021:33 claims the order "body → row → state → agent → record"; body-vs-row and body-vs-state are untested. *Smallest fix:* one bad body against an unknown id expecting `422 bad_body`, not `404` — that is the ordering that actually costs a client.

**SP4 · nit — D1's "steer-specific sentence" is untested.**
`assert_envelope` (`tests/steer.rs:79-90`) checks `code` only, so `bad_steer`'s message could be `bad_prompt`'s and every test passes — while the whole justification for a second constructor (`error.rs:161-171`, ADR-0008) is the sentence. *Smallest fix:* assert `body["message"]` on one 422.

**SP5 · nit — `parse_steer` has no unit test where `parse_prompt` has one** (`sessions.rs:807-829`). Integration coverage exists, so this is consistency only.

**SP6 · nit — the ordering comment overstates what the log proves.** `sessions.rs:522-532` and ADR-0021:39-43 say the log "holds what was delivered", but with the stub (and the Docker backend refusing, `docker.rs:55-57`) nothing can report a turn that ended under the read, so a `turn.steer` can be recorded for a turn that is already over. Documented as a trade in D4; report only.

**Spec verdict: not clean — 1 should-fix (SP1: an acceptance bullet the story claims but does not prove), 2 consider, 3 nit.** Every acceptance bullet is *implemented* correctly; one is not *proved*.

---

### Is ADR-0021's exception to ADR-0018 defensible?

**Defensible — yes, but one of its supporting claims is overstated.** ADR-0018's rule ("A message id is the key", `0018:37-40`) exists to fold a *stream* into one block; a steer is one frame that nothing continues, and the renderer's map is filled at creation (`main.ts:493-500`), so an id would key a block nothing updates. Minting `steer-<instant>-<n>` would invent a third id namespace for zero present use — the ADR is right to refuse it.

The overclaim is ADR-0021:28-29: *"a frame that names no id is not a block a client has ever keyed by"* — true of the **client**, false of the **log**. The event log is durable and replayed (`events/repository.rs`, ADR-0013/0014), so if a later story gives `turn.steer` a continuable shape and adds an id, the same table holds two shapes of one kind forever, and every reader must branch. That is the real price of the exception, and it is paid by HS-032 rather than by this story. One alternative the ADR does not consider: key an unkeyed block by the frame's own **seq** (already the SSE `id:`, ADR-0014), which *generalizes* ADR-0018's rule to "message id or seq" instead of excepting it — cheaper in doctrine, and it costs `parseFrame` a parameter it does not have today. Verdict: keep the decision, soften the sentence to admit the two-shapes-in-one-log cost.

### Did the story silently grow?

**No.** No steer queue; no new `Trigger`, no table row, `sessions/state.rs` untouched by the diff; Docker still refuses every operation (`backend/docker.rs:55-57`, unmodified); no `message.snapshot` interaction (spec item 15's compaction only rewrites message deltas, `HolosmithSpec.md:257`); `last_active_at` reuses HS-015's existing `touch` method rather than adding a second path. `SessionBackend::steer` gaining its first caller is exactly ADR-0020:130's deferred item.

### Landmines (brief's list)

| Landmine | Result |
|---|---|
| No lock across an `.await` | Pass — `one_locked` returns owned; `touch` sync; `record_steer` awaits with no guard held. |
| Event broadcast after `COMMIT` | Pass — handler only queues (`events/mod.rs:82-87`); broadcast is the writer's (`events/writer.rs:82-90`). |
| No state write outside `Transitioner` | Pass — `touch` is the activity column; the source walk still passes and its non-vacuity assertion is untouched. |
| Source walk proves it found something | Pass (pre-existing, `state.rs:710-721`), and the new code does not trip it. |
| Tests prove what they claim | **Fail on SP1** (a guard that passes with the behaviour removed); SP2/SP3/SP4 are weaker-but-real gaps. |

### Commands a supervisor must run (I have no shell)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                                   # includes the new tests/steer.rs
(cd web && npm test)                         # node --test, the two new transcript tests
scripts/verify.sh                            # full gate; the two new serve cases bring it to 30
```
Mutation check for SP1 (the finding's proof):
```
# after deleting `touch(state, &current.id);` at crates/holosmith/src/http/sessions.rs:534
cargo test --test steer                      # expect: still green ⇒ SP1 confirmed
```
And the S3 check:
```
grep -n 'steer' web/src/style.css            # expect: no matches
```

Note: `docs/review/` holds one artifact per story (`PER-33.md` … `PER-48.md`). My brief is read-only, so I have not written `docs/review/PER-49.md`; this report is the artifact.

**Merge verdict: OK with notes** — no P0, and the shipped behaviour is correct on both axes. Fix **S1**, **S2** and **SP1** before this is called done; the rest are report-only.
## Resolution

Every finding was triaged. The reviewer's SP1 was checked by mutation first — deleting the handler's
`touch` left the steer tests green, which confirmed the assertion was satisfied by the settle rather
than by the steer. It is fixed in the commit that carries this table, where the same mutation now fails
the test.

| # | Axis | Finding | Verdict |
| --- | --- | --- | --- |
| S1 | Standards, should-fix | ADR-0018's supersession chain was not extended, and ADR-0020 still said `steer` had no caller — the same nit that was fixed one story ago | **Fixed.** ADR-0018 gains "**Superseded by ADR-0021:** steer is wired too …", and ADR-0020's caller count now points at ADR-0021 |
| S2 | Standards, should-fix | The composer's own doc comment still said steering "keeps the notice" | **Fixed.** "All three post for real: the prompt (HS-015), the abort (HS-016) and the steer (HS-017)" |
| S3 | Standards, consider | `article.steer`, `.steer-label` and `.steer-text` had no rules in `style.css`, and without `white-space: pre-wrap` the operator's line breaks vanished | **Fixed.** Rules added next to the usage and tool blocks, in their style: a `currentColor`-mixed left border, a quiet uppercase label, `pre-wrap` on the text |
| S4 | Standards, nit | Two near-verbatim `touch` helpers (`turn.rs` and the steer handler) | **Fixed.** One `crate::sessions::activity::touch(database, id)` owns the lock and the "log it, carry on" rule; both callers use it |
| S5 | Standards, nit | `parseFrame`'s steer branch re-implemented the JSON parse and object guard below it | **Fixed.** The kind list and the parse are hoisted above the dispatch, and the steer branch reads `fields["text"]` like every other branch |
| S6 | Standards, nit | Three new comments said "injected", a word **Steer**'s glossary entry lists under *Avoid* | **Fixed.** "delivered", the ADR's own word; the Linear bullet is still quoted verbatim in ADR-0021's opening paragraph, where it is a quotation rather than vocabulary |
| S7 | Standards, nit | The steer's failure was logged twice (a `warn!` and then `internal`'s `error!`) | **Fixed.** The `warn!` is gone; `internal` logs once |
| S8 | Standards, nit | ADR-0021 named a `register_steer` symbol that does not exist | **Fixed.** "The steer handler" |
| S9 | Standards, nit | ADR-0021 and `docs/build.md` claimed `case_steer` "reads it again for the settle", where the script polls the row | **Fixed.** Both now say "polls the row until it says `idle`" |
| S10 | Standards, nit | `wait_for_idle` returned 1 on a failed fetch instead of printing `FAIL`, breaking `docs/build.md`'s promise | **Fixed.** It fails through `fail "$label/no_answer" …` like every case |
| S11 | Standards, nit | Two doc drifts in `http/error.rs` (the "same code again" link, and "HS-017 will name it too") | **Fixed.** `[Self::bad_body]` linked; "HS-017 names it too (a steer with no turn)" |
| SP1 | Spec, should-fix (P1) | The `last_active_at` assertion could never fail on the steer's behalf: the clock is whole seconds, so the settle's own touch satisfied it — HS-015's "updates on … steer" was claimed but unproven, and **confirmed by mutation** | **Fixed.** The row is pushed 60 s into the past through the repository, the steer is sent, and the value is read immediately after the 202 — the handler touches before it answers and the twelve-frame turn cannot have settled. Removing the handler's `touch` now fails the test |
| SP2 | Spec, consider | The two 409 tests asserted the envelope only; a handler that steered before checking state would pass | **Fixed.** Both refusals now assert the stub recorded no `Call::Steer` |
| SP3 | Spec, consider | The "bad body before anything else" test only proved body-before-agent | **Fixed.** A bad body against an unknown id must be `422 bad_body`, not `404` — the ordering that costs a client |
| SP4 | Spec, nit | `bad_steer`'s sentence is not asserted, so the second constructor's whole justification is untested | **Dismissed.** The repository's rule is that **no test pins a message sentence** (the landmine list; ADR-0008 makes `code` the API and the sentence prose). The constructor is covered by the unit test's check that the sentence names `guidance`, which is a shape assertion rather than a pinned string |
| SP5 | Spec, nit | `parse_steer` had no unit test where `parse_prompt` has one | **Fixed.** `a_steer_body_is_one_non_empty_guidance_and_nothing_else`, twelve shapes |
| SP6 | Spec, nit | With the stub (and Docker refusing), a `turn.steer` can be recorded for a turn that ended under the read | **Dismissed, as reported.** It is D4's documented trade: the record follows a delivery the backend confirmed, and a backend that confirms a delivery to a dead turn is a backend defect that ADR-0021's "the agent is asked first" already forbids |

The reviewer's judgement on ADR-0021's exception to ADR-0018 was "defensible, but one claim is
overstated". The claim — "a frame that names no id is not a block a client has ever keyed by" — was true
of the client and false of the durable log, so ADR-0021 now states the real price: a `turn.steer` that
grew an id later would leave two shapes of one kind in the same table, and the cheaper generalization is
to key an unkeyed block by the frame's `seq`, which already travels as the SSE `id`.

Gates re-run after the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` (209 lib + 222 across twenty integration binaries), `cd web && npm test` (17),
`scripts/verify.sh` (30 cases + the CORS sweep, 126 assertions).
