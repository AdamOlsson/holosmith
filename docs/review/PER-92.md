# PER-92 — HS-060 message snapshots and compaction

**Fixed point:** `1fb7454` (a test-only fix: one preflight test at a time, because a fork inherits a shim's
write descriptor — the `ETXTBSY` flake that was reddening the gate at random). **Change reviewed:**
`1fb7454...f0ecf8c` (`5c0f960`, `e2a24b6`, `f0ecf8c`).
**Verdicts:** Standards — **PASS with findings** (no P1; 3 P2, 5 P3); Spec — **PASS** (no findings).

Saved verbatim from the `reviewer` child, then triaged below.

---

## Review

Read: the brief, the diff artifact (`/tmp/holosmith/reviews/PER-92-diff.txt`, 3 commits), the story slice (HS-060), spec item 15 (`HolosmithSpec.md:257`), ADRs 0013/0018/0025/0027/0031/0032, `GLOSSARY.md`, `PROMPT.md`, `docs/build.md`, and the seams the diff touches (`events/repository.rs`, `events/mod.rs`, `compactor.rs`, `commands/serve.rs`, `http/app.rs`, `tests/compaction.rs`, `scripts/verify.sh`, `web/src/transcript.ts`).

---

## Standards

### S1 · P2 · Two ADRs' stated invariant is now false and neither is amended
`docs/adr/0027-the-token-budget.md:18-20` — "The price is stated rather than hidden: **the events repository looks inside one kind's payload**; every other kind is opaque to it, and `usage()` is the only function in the file that parses one."
`docs/adr/0031-coalescing-in-the-writers-batch.md:43-44` — "The repository's invariant is untouched — it still parses exactly one kind."
`docs/stories/README.md:149` repeats it: "the repository still parses exactly one, ADR-0027".

The diff makes `events/repository.rs` parse three payload shapes: `usage()` (message.end, unchanged), `parse_delta` on `message.delta` rows (`repository.rs:319`) and `message_id()` on `message.end`/`message.snapshot` rows (`repository.rs:108`, called at `:287`, `:297`), plus it *writes* a payload (`repository.rs:372`). ADR-0032 records reusing the writer's parser (`docs/adr/0032:30-31`) but never says ADR-0027's invariant moved. `PROMPT.md` rule 2 makes the ADRs canonical and step 5 says "update the docs, don't leave them lying".
**Smallest fix:** one sentence in ADR-0032 — "the events repository now parses three payloads: `message.end`'s `usage`, a delta's pinned shape, and a `message_id`; ADR-0027's 'exactly one kind' is superseded" — plus a matching amendment line in ADR-0027 and the `docs/stories/README.md:149` parenthetical.

### S2 · P2 · `sweep` throws away `Compaction::unreadable`, so the ADR's "the count is reported" is false on the only production path
`crates/holosmith/src/compactor.rs:123-132`:
```rust
Ok(compaction) if compaction.messages > 0 => { tracing::info!(...) }
Ok(_) => {}
```
`repository.rs:344-349` returns `Compaction { unreadable: n, messages: 0, .. }` and writes nothing. `docs/adr/0032:33` says of exactly this case: "nothing is written, and the count is reported". The count reaches only a direct `fold` caller — which is a test. In production the session is then never folded again for as long as that row exists (the guard is session-wide, `repository.rs:319-337`, and is not scoped to the candidate messages), with no log line at all, so the failure mode is "compaction silently stops for this session" — the unbounded growth the story exists to prevent, with no signal.
**Smallest fix:** one arm before the catch-all:
```rust
Ok(compaction) if compaction.unreadable > 0 => { tracing::warn!(target: "holosmith::compactor", session = %id, unreadable = compaction.unreadable, "a delta the contract does not describe stopped this session's compaction"); }
```

### S3 · P2 · The fold holds the process-wide DB mutex across an unbounded scan and one `DELETE` per removed row
`compactor.rs:172-188` takes `Arc<Mutex<Database>>` and holds it for the whole `compact` call; the ADR's stated bound is on *messages* (`compactor.rs:56`, `MESSAGES_PER_PASS = 500`; `repository.rs:304` truncates candidates). Nothing bounds the *work*: `DELTAS` (`repository.rs:67-69`) has no seq bound and no `LIMIT` — it reads and JSON-parses **every** delta row in the session, including those of messages inside the window — and `repository.rs:359-375` issues one `DELETE` statement per removed row (the comment at `:70-71` prefers that to an `IN` list for stylistic reasons). On HS-061's stated target (a 200k-event session) that is a multi-hundred-millisecond mutex hold every 60 s for every non-`running` session — the mutex the writer and every handler need.
**Smallest fix:** bound the delta scan to the region being folded — have `COMPLETED` also select `seq` and pass the highest candidate's `message.end` seq as `AND seq < ?4` on `DELTAS`. That keeps the "no `LIMIT` in the candidate query" argument intact and removes the scan over fresh messages' deltas.

### S4 · P3 · `GLOSSARY.md` now contradicts itself about seq reuse
`GLOSSARY.md:255` (Seq): "assigned by the event writer from the session's `last_seq` counter and **never reused**." `GLOSSARY.md:328` (new Message snapshot entry): "it takes the highest seq among the delta rows it replaces, **which is the one place a seq is reused**." GLOSSARY outranks the ADRs in `PROMPT.md` rule 2, so the contradiction is worth a line.
**Smallest fix:** amend the Seq entry to "…and never reused, except by a **message snapshot**, which takes the highest seq of the rows it removed."

### S5 · P3 · `Delta` is widened further than the new reader needs
`events/mod.rs:154-157` makes the struct and all three fields `pub(super)`. `events::repository` is a *child* module, so private items of `events` were already visible to it — the same mechanism the diff relies on for `super::parse_delta`, which stayed private. `pub(super)` from `events` resolves to the crate root, i.e. `Delta` becomes crate-wide public for no reader that needs it.
**Smallest fix:** revert to `struct Delta` with private fields.

### S6 · P3 · `AppState::compactor`'s doc describes an `Option` it does not return
`http/app.rs:158` — "`Some` always: the window is the spec's 24 hours…" while `app.rs:164` is `pub fn compactor(&self) -> Compactor`. Reads as a leftover from the `sweeper()` shape.
**Smallest fix:** "Always present: …".

### S7 · P3 · `serve.rs`'s shutdown comment contradicts itself
`commands/serve.rs:210-213` — "compaction writes, so a pass that started must not be cut off in the middle of a transaction. `abort` is what the runtime would do anyway". `abort` *does* cut a pass off mid-transaction; what makes it safe is that the `Transaction` rolls back on drop. As written the sentence argues for and against the same line.
**Smallest fix:** "…so a pass that started is not allowed to leave a half-written transaction: `abort` drops the task, and the transaction rolls back."

### S8 · P3 · Judgement call (smell baseline: Duplicated Code) — the "identical transcript" proof is a second renderer
`tests/compaction.rs:190-240` re-implements `web/src/transcript.ts`'s fold rules in Rust (`transcript()`), and that model is the whole proof of acceptance line 5. The file acknowledges it, and the page's own parser is pinned separately (`web/test/transcript.test.ts:59-82`, same key order), so the pair does hold — but ADR-0031's precedent for the identical claim was "the coalescer's exact payload bytes are fed through the page's own frame parser" (`docs/adr/0031:...` §"What the gate can and cannot show"), i.e. compose, don't mirror. Report-only: if the page's snapshot rules change, this test stays green.

### Standards verdict: **PASS with findings** (no P1; S1–S3 are P2 and each is a small, local fix)

---

## Spec

Mapped each verbatim acceptance line to the code, and found no missing, partial, or wrongly-implemented requirement.

| Acceptance (story line 227-231) | Implementation | Evidence |
|---|---|---|
| "replaces delta rows of messages completed more than 24 hours ago with one `message.snapshot` event holding full text, thinking and message id" | `compact` selects `message.end` rows with `created_at < cutoff`, folds text/thinking separately, writes one `{"message_id","text","thinking"}` | `repository.rs:56`, `:288-303`, `:372-378`; `SNAPSHOT_AFTER` = 24 h at `compactor.rs:41`; byte-exact payload asserted in `tests/compaction.rs:...` (`{"message_id":"m1","text":"Hello, world","thinking":"thinking"}`) |
| "takes the highest seq of the removed rows, so seq stays strictly increasing with gaps only where compaction removed rows" | `fold.seqs.last()` (rows pushed in `ORDER BY seq ASC`) is the insert seq; `sessions.last_seq` neither read nor bumped | `repository.rs:359`, `:375`; `sessions/repository.rs:62-63` untouched; test `the_snapshot_takes_the_highest_removed_seq_and_the_counter_does_not_rewind` (mutated in the brief: `first()` fails only it) |
| "Clients apply a snapshot by message id, replacing rather than appending, and it is safe to receive after partial deltas" | The shipped client already replaces by id; payload key names match its parser | `web/src/transcript.ts:184-193` (absent kind = empty string), `main.ts:472-477` (`replaceKind`); `web/test/transcript.test.ts:59-82` |
| "per session, batched, and never runs while a turn is running for that session" | one `fold` per session id, one transaction per session, bounded by `MESSAGES_PER_PASS`; `foldable` excludes `running`/`starting`/`deleting`, and the row is re-read **inside** the same mutex `Transitioner::apply` takes | `compactor.rs:196-198`, `:172-188`; `sessions/state.rs:369` locks the same `Arc<Mutex<Database>>` — so "before the pass" and "after the pass" really are the only two cases |
| "A test replays a client through a compaction boundary and asserts an identical transcript" | `a_replay_through_the_compaction_boundary_renders_the_same_transcript`, with a non-vacuity assertion (`after_log.len() < rows_before`) and the expected transcript pinned literally | `tests/compaction.rs` |

No scope creep: every file in the diff serves the story (module, repository statements, one `serve` wiring site, one `AppState` accessor, tests, gate case, ADR/glossary/build.md/stories README). Nothing is broadcast, no config key was invented, no schema change — the three decisions that could have gone either way are recorded in ADR-0032 and match the code (`docs/adr/0032:14-19`, `:22-35`, `:37-51`, `:53-62`).

### Spec verdict: **PASS** (no findings)

---

## Verified by reading

| Claim | Where |
|---|---|
| The 24-hour window is a constant, not a key, and the constructor takes it only for tests | `compactor.rs:41`, `:78-86`; `http/app.rs:164-166` |
| `foldable` = `idle \| stopped \| error`, i.e. the story's "never while a turn is running" | `compactor.rs:196-198`, unit-tested at `:236-245` |
| The mutex compaction holds is the one the transitioner takes, so the re-read is the authority | `compactor.rs:172-188` vs `sessions/state.rs:360-369` |
| Compaction writes no session state at all (so "no state change outside `Transitioner::apply`" holds) | `repository.rs:256-390` touches `events` only; `sessions().get`/`list` are reads |
| Every new `events` statement is in `events/repository.rs`, the file the guard maps to the table | `repository.rs:56-72`, `:287-330`, `:344-390`; guard is table→file in `sql.rs:197-203` |
| The snapshot payload's field order is ADR-0018's | `repository.rs:88-93` (`message_id`, `text`, `thinking`) vs `docs/adr/0018:18` |
| "A snapshot is the last frame for its id" is not violated: `message.end` (not a delta) follows it | `docs/adr/0018:41-44`; the fold reuses the max removed seq, so no delta can follow |
| Nothing is broadcast, and a reconnecting client reads the snapshot from `read_since` | `docs/adr/0032:53-62`; `events/stream.rs:126-139` |
| The gate case's expected text is exactly what the stub emits, so the assertion is satisfiable | `scripts/verify.sh:1612` vs `backend/stub.rs:302-323` (`whole` is those four text deltas concatenated, with the tool frames between them not contributing text) |
| `case_compaction` boots its own server and guards `python3` before `stop_serve`, matching `case_worktree`'s rule; every case after it boots its own or serves nothing | `verify.sh:1614-1625`; later cases at `:1717`, `:1897`, `:1813` |
| The Rust transcript model and the page's parser agree on the snapshot payload shape | `tests/compaction.rs:190-240` vs `web/src/transcript.ts:184-193` |
| ADR-0032's claims about the code (per-session pass, bound applied after the folded filter, no `last_seq` bump) | `docs/adr/0032:37-51`, `:53-56` vs `repository.rs:277-305`, `:359-378` |
| `docs/build.md`'s new operator-facing paragraph matches the code (tick, one transaction per session, the three skipped states, gaps + `last_seq + 1`) | `docs/build.md` (event-log paragraph) vs `compactor.rs:52-56`, `:196-198`, `sessions/repository.rs:62-63` |

## Could not verify without running

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (231 lib + 10 in `tests/compaction.rs`) — taken from the brief, not observed.
- `scripts/verify.sh` on the post-commit tree: the new case count/assertion total, that `case_compaction` actually reaches `1 0` rows and the replay assertions pass, and that the CORS sweep still passes over its new answers.
- The four mutation claims (removing the fold's re-read, `first()` for `last()`, dropping the already-folded filter, removing the unreadable guard) — each is stated in the brief and each maps to a test I read, but the mutations themselves were not run.
- Runtime cost of S3 (mutex hold per tick on a large session) and whether `unreadable` can realistically occur (it depends on producers not yet written, HS-034).
- `npm test` for `web/` — no web file is in the diff, so this is only the standing gate.

## Merge verdict: **OK with notes**

Both axes pass. Nothing blocks: no P1, no correctness or spec failure. Recommend folding S1 and S2 into the fix pass — they are one sentence and one match arm respectively, and S2 in particular converts a silent permanent stall into a log line. S3 is worth a decision (bound the delta scan) rather than a silent deferral, because HS-061's target session size is exactly where it bites.

## Resolution

No P1, and the spec axis came back clean. The three P2s were all real and all cheap; two of them changed
behaviour, and one of those changed what a test could see — which is the honest kind of finding.

| Finding | Triage | How / why |
| --- | --- | --- |
| **S1 P2** — ADR-0027 and ADR-0031 both state "the events repository parses exactly one kind's payload", and ADR-0031 repeats it in `docs/stories/README.md`; the diff makes the repository parse three payload shapes and write one, and neither ADR is amended | **Fixed** | ADR-0027 gained an "Amended by ADR-0032" paragraph (the *argument* stands, the *count* is three); ADR-0031's sentence now says the invariant was untouched by *that* change and names ADR-0032 as what widened it; the README parenthetical says so too; and ADR-0032's second section states the three parses and the one write explicitly. |
| **S2 P2** — `sweep`'s `Ok(_) => {}` discards `Compaction::unreadable`, so ADR-0032's "the count is reported" was true only of a direct `fold` caller; in production a session would stop being compacted for ever with no log line at all | **Fixed** | A `warn!` arm before the catch-all names the session and the count. That is the whole fix, and it converts a silent permanent stall into a line an operator can act on. ADR-0032 says why it is a `warn` and not a `debug`. |
| **S3 P2** — the fold holds the process-wide DB mutex across an unbounded delta scan (no seq bound, no `LIMIT`) plus one `DELETE` per row, so on HS-061's 200k-event session it is a multi-hundred-millisecond mutex hold every minute | **Fixed, and it narrowed the strictness rule** | `COMPLETED` now also selects `seq`, and `DELTAS` is bounded by `AND seq < ?3` where `?3` is the newest candidate's `message.end`: a message's deltas all precede its own end, so that is the furthest row a fold can reach. The per-row `DELETE` stays — it is exact, and it is now bounded by what the scan returned. **Consequence the reviewer did not name:** the strictness guard now fires only for a malformed delta *inside* the scanned region, which is exactly the set that could belong to a message being folded; a malformed delta above the bound belongs to a message still inside the window and a later pass will see it. The test moved the bad row before the `message.end` (where such a row can be), and `DELTAS`' doc comment and ADR-0032 both record the narrowing. |
| **S4 P3** — `GLOSSARY.md` contradicted itself: **Seq** said "never reused", **Message snapshot** said "the one place a seq is reused" | **Fixed** | **Seq** now reads "…never reused — except by a **message snapshot**, which takes the highest seq of the delta rows it removed". |
| **S5 P3** — `Delta` was widened to `pub(super)`, which from `events` means the crate root, and `repository` is a child module that already sees private items | **Fixed** | Reverted to a private struct with private fields, and the doc comment says why (one parser, two readers, both inside `events`). `parse_delta` stayed private throughout and is the proof the mechanism works. |
| **S6 P3** — `AppState::compactor`'s doc said "`Some` always" while it returns a `Compactor` | **Fixed** | "Always present, unlike [`Self::sweeper`]". |
| **S7 P3** — `serve.rs`'s shutdown comment argued for and against the same line ("must not be cut off in the middle of a transaction" / "`abort` is what the runtime would do anyway") | **Fixed** | It now says what actually makes it safe: `abort` drops the task at its next await point and the transaction rolls back on drop — which is also why the fold itself holds no await. |
| **S8 P3** — the Rust `transcript()` model in `tests/compaction.rs` mirrors `web/src/transcript.ts` rather than composing with it, so a change to the page's snapshot rules would leave that test green | **Dismissed, with the page's own test and the demo as the other half** | The page's snapshot rule is already pinned by its own test (`web/test/transcript.test.ts`, "a snapshot replaces by message id, safe after partial deltas", including the absent-kind-is-empty rule the Rust model relies on), so the two halves are pinned separately rather than unpinned. The composition the reviewer wants — the page's parser applied to a real post-compaction replay — is the demo's Chromium half, which renders the session screen from the artifact's own log and asserts the whole text, the whole thinking, one block and no duplication; it is a demo and not a gate because no gate may require a browser (`docs/build.md`). Making it a third node test over a hardcoded payload would restate the existing node test. |

Re-run after the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
(231 lib, 10 `tests/compaction.rs`), `cd web && npm test` (30), `cd web && npx tsc --noEmit`, and
`scripts/verify.sh` — all green on a clean tree, and the demo re-run afterwards. The four mutation proofs
were re-run against the changed queries and still fail exactly one test each: `first()` for `last()` →
the seq rule; no already-folded filter → the limit; no unreadable guard → the strictness test; no state
re-read → the authority test.
