# PER-91 — HS-059 event coalescing

**Fixed point:** `c233ab7` (the PER-58 fix commit). **Change reviewed:** `c233ab7...65f7107` (`06f506b`, `65f7107`).
**Verdicts:** Standards — **BLOCK on S1** (1 P1, 3 P2, 7 P3); Spec — OK with notes (2 P3).

Saved verbatim from the `reviewer` child, then triaged below.

**Identifier correction.** The work shipped is HS-059, whose issue is **PER-91**; the two code commits and
this file were first written against **PER-93**, which is HS-061 (history truncation). The reviewer's text
below said `PER-93` throughout for the same reason — the brief it was given carried the wrong identifier —
and it is filed here, under the story it actually reviews. The commit trailers still say `Closes PER-93`;
the Linear record is PER-91, with a correction note on PER-93.

---

## Review

**Target:** PER-91 (HS-059 — event coalescing), fixed point `c233ab7`, diff artifact `/tmp/holosmith/reviews/PER-91-diff.txt` (`c233ab7...HEAD`, commits `06f506b`, `65f7107`).
Working tree is identical to reviewer-launch HEAD `65f7107` (watchdog: no diff), so the artifact is the shipped change.

I read in full: the diff; `crates/holosmith/src/events/{mod.rs,writer.rs,repository.rs}`; `crates/holosmith/tests/{events.rs,turns.rs,streams.rs}`; `crates/holosmith/src/backend/stub.rs`; `web/test/transcript.test.ts`; `docs/adr/0018`, `0027`, `0030`, `0031`; `HolosmithSpec.md` item 15; `docs/stories/phase-2-deployable-mvp.md` HS-059/HS-060; `GLOSSARY.md`; `docs/build.md`; `scripts/verify.sh`; `AGENTS.md`; `PROMPT.md`.

---

## Standards

**S1 — P1. `tests/turns.rs` asserts an invariant ADR-0031 documents as false, so the story's live-path proof is green by timing luck.**
`crates/holosmith/tests/turns.rs:475-484`:

```rust
for pair in events.windows(2) {
    ...
    assert!(
        one["message_id"] != two["message_id"] || one["kind"] != two["kind"],
        "two consecutive events are deltas of one message and kind: {pair:?}"
    );
}
```

The comment three lines above it (`turns.rs:471-474`) says the opposite — "two rows of one message and kind may still sit next to each other across a batch boundary" — and so does the ADR this change ships: ADR-0031, "Four boundaries, and a frame is one of them": *"A consequence worth stating because it surprised the test that first asserted otherwise: after a different commit, two rows of one message and kind may still sit next to each other in the log. Coalescing is a rule about one batch; the log's adjacent rows are not."* If a run straddles the writer's 25 ms deadline, the log holds exactly the two adjacent same-`(message_id, kind)` delta rows this loop forbids, and the loop fails on a correct implementation. The same luck carries `assert_eq!(deltas.len(), 3)` (`turns.rs:511-515`) and the kinds array (`turns.rs:517-526`). It passes today only because `paced(0)` (`turns.rs:438`) makes the stub's 12 frames land inside one 25 ms window (`writer.rs:100-131`).
*Smallest fix:* delete the `windows(2)` loop, the `== 3`, and the kinds array; keep `deltas.len() < 7`, "one turn is one message id", and the first/last-sentence assertions, and note that the deterministic merge is proved in `tests/events.rs:590` (5,000 → few) and the `mod.rs` unit tests. The ADR already says the log's adjacency is not the rule; the test should not assert it.

**S2 — P2. `coalesce` re-reads and re-writes the accumulated payload once per delta: cost grows with the square of a run's text — the exact shape ADR-0018 rejects on the renderer side.**
`events/mod.rs:191` (`&& let Some(before) = delta(&previous.payload)`) parses the run's whole accumulated JSON every iteration, and `events/mod.rs:203` serialises it again. For a run of *n* deltas averaging *d* bytes, the writer does ≈ *d·n²* bytes of parse+serialise work inside one batch, on the single `std::thread` every session shares; a 100 KB message arriving as 1,000 × 100-byte deltas is ≈100 MB of work per batch. The batch is not window-bounded either — `collect`'s post-deadline drain (`writer.rs:126-131`, "the window is a deadline for starting the commit, not a limit on what the commit contains") can pull thousands of frames. ADR-0018 makes precisely this argument against `node.textContent += piece`: *"reads the accumulated text and writes it back, so one turn's rendering cost grows with the square of its own length."*
*Smallest fix:* keep the open run outside the vec — `Option<(usize, Delta)>` holding the last row's index and its accumulated `Delta` — append `this.text` to that value, and serialise into `merged[index].payload` once when the run ends.

**S3 — P2. `coalesce` is `pub`, widening the crate's public API with no caller outside it.** `events/mod.rs:181`. Only `events/writer.rs:61` and the module's own `#[cfg(test)] mod tests` call it; `writer` is a child of `events`, so `pub(crate)` (or `pub(super)`) suffices. The module's own pattern is minimal visibility: `pub(crate) mod stream` (`events/mod.rs:12`), `mod writer` (`13`), `fn usage` private (`repository.rs:28`), `pub(crate) fn scratch_writer` (`mod.rs:313`). `holosmith::events::coalesce` is now part of the external contract for no reader. *Fix:* `pub(crate) fn coalesce`.

**S4 — P2. The shape check is looser than ADR-0031's rule, and a merge silently drops unknown fields — contradicting "the writer is not a validator".**
`events/mod.rs:156-163` accepts any JSON object that *contains* the three string keys and ignores everything else; `events/mod.rs:197-203` then re-serialises from `Delta`, discarding anything extra. ADR-0031, "The writer reads a second payload, on purpose": *"A payload that is not the pinned shape is **never merged**: it is written exactly as the producer sent it … rewriting a payload it does not understand would be inventing an event the producer did not emit."* `{"message_id":"m1","kind":"text","text":"a","x":1}` is not the pinned shape, yet it is merged and `x` vanishes. Latent today — no producer emits a fourth key (`backend/stub.rs:284` is the only delta spelling) — but it is the one place the code is weaker than the ADR it ships.
*Smallest fix (2 lines):* in `delta`, `let object = event.as_object()?; if object.len() != 3 { return None; }`. Or amend the ADR sentence to "a payload missing one of the three keys".

**S5 — P3. A tautological assertion claims the malformed-payload test's conclusion.** `tests/events.rs:723-732`: `take(3)` (`events.rs:104-113`) already panics unless three rows arrive, so `assert_eq!(events.len(), 3, "a malformed payload broke the run, as it must")` can never fail on its own; the real proof is that `take(3)` completes (and a cross-run merge would deliver 2 rows and time out after `PATIENCE`, 10 s, with the misleading message "the writer commits within the deadline"). *Fix:* drop the `events.len()` assertion and say in the comment that three arrivals are the proof.

**S6 — P3. Two different `delta` functions in one file, the test one shadowing the parser via `use super::*`.** Parser at `events/mod.rs:156` returns `Option<Delta>`; test helper at `events/mod.rs:318` builds a `NewEvent`. The name of a parser that returns a struct reads as a noun. *Fix:* rename the parser `parse_delta` (mirrors `usage()` in `repository.rs:28`), or the struct `DeltaPayload`.

**S7 — P3. The writer's failure log now counts rows, not the events it lost.** `events/writer.rs:71` (`count = batch.len()` inside `tracing::error!`) previously counted appended events; after `writer.rs:61` it counts coalesced rows, so a dropped batch of 5,000 deltas logs `count=3` — and one dropped row can carry a whole run's text. *Fix:* keep the pre-coalesce length and log both, or rename the field to `rows`.

**S8 — P3. The glossary entry is in the wrong section.** `GLOSSARY.md:345` puts **Coalescing** between **Worktree** (`341`) and **Refused project** (`349`) under `### Work and results` (`339-392`), whose neighbours are branches, merges and publishes. Its subject is the event writer's batch: `### Events and history` (`248-338`) holds **Delta** (`278`), **Event writer** (`258`), **Seq** (`254`) and **Delta kind** (`315`). *Fix:* move the entry there, after **Delta**.

**S9 — P3. `scripts/verify.sh`'s own usage text does not mention the case it now runs.** The header comment (`verify.sh:12-20`) enumerates what the `serve` phase proves and `usage()` (`verify.sh:32-34`) prints it as `-h` output; it gains no line for coalescing, while `docs/build.md:167` and ADR-0031 both describe `case_event_coalescing`. *Fix:* add "a turn's deltas coalesced" to that sentence.

**S10 — P3. The change makes two pre-existing assertions silently pace-dependent, and neither says so.** `tests/turns.rs:279` (`assert_eq!(deltas.len(), 7)`) and `scripts/verify.sh`'s `case_prompt_turn` kinds string (7 × `message.delta`) hold only because the stub paces one frame per 200 ms (`backend/stub.rs:38`) — eight commit windows. Before HS-059 no pace could change a row count; now a pace below 25 ms would break an unrelated case with a message about frame order. ADR-0031 and `docs/build.md` explain the gate half; the test half is unremarked. *Fix:* one comment at `turns.rs:279` and one at `case_prompt_turn`'s kinds assertion naming the 25 ms batcher as the reason the seven are seven.

**S11 — P3. A vacuous assertion in the renderer test.** `web/test/transcript.test.ts:526`: `assert.equal(appended(split, "m1", "thinking"), "")` — `split` contains no `thinking` delta, and `appended` (`transcript.test.ts:38-46`) returns `""` for any change list without match, so the line holds for every possible input, while the comment above it claims something about the renderer's kind nodes. *Fix:* delete it, or assert something falsifiable about the thinking node (a thinking delta lands in `thinking` and not in `text`).

**Standards verdict: BLOCK on S1** (one P1: the story's live-path proof asserts a documented-false invariant). S2–S4 are small, contained edits; S5–S11 are notes. Everything else checks out: `coalesce` is pure and documented against its ADRs, the module/ADR/glossary/build.md prose matches the code it describes, the new `case_event_coalescing` follows the harness's shape (`local`, `fail`/`expect_status`/`check`, `assertions` counted once per claim) and its placement rationale in its own comment is real (`max_live_sessions = 1` in `docs/build.md:104`), and no smell from the baseline survives the "repo overrides" rule (the second payload parser is ADR-0031's stated decision, not an accidental duplication).

---

## Spec

Spec item 15, first bullet (`HolosmithSpec.md:256`): *"Coalescing: inside each 25 ms commit, consecutive deltas of the same kind and message become one row with one seq."*

- **Acceptance 1 — met.** `events/mod.rs:181-206` merges consecutive `message.delta` rows in the writer's batch (`writer.rs:61`, between `collect` and `commit`), one row and one seq per run; the session boundary is a fourth condition the spec does not name and cannot contradict (it can only merge less). The `created_at` of the first event is kept and pinned (`mod.rs:345-350`).
- **Acceptance 2 — met.** Kinds and message ids are compared at `mod.rs:192-193`, and the run also ends on any interleaved event, by construction and by test (`tests/events.rs:665-700` asserts `["a","b","c","d","tool.start","ef"]`, which fails if either guard is removed, including the session guard — removing it merges `c`+`d`).
- **Acceptance 3 — met, by composition rather than end-to-end (P3, SP1 below).**
- **Acceptance 4 — measured; "reported in the story" is a ship-time step outside this diff (P3, SP2 below).**

**SP1 — P3. Nothing feeds a coalesced payload through the renderer; the two halves of "byte-identical" are pinned separately.** `web/test/transcript.test.ts:506-527` compares a hand-built merged frame against hand-split frames (`:521`), never the writer's output: `coalesce`'s byte shape is pinned at `events/mod.rs:345-349` and the parse of that shape at `transcript.test.ts:47-56`, so the seam composes — but no test closes it. Dismissible with that reasoning recorded; if a second path is wanted, one node test parsing `'{"message_id":"m1","kind":"text","text":"a b"}'` through `Transcript.apply` is enough.

**SP2 — P3. The row-count reduction is reported in ADR-0031 ("5,000 deltas become three rows") and `docs/build.md:167`, not in the story slice.** `docs/stories/phase-2-deployable-mvp.md:214-215` still reads `- [ ]`; no story file in this repo is ever ticked (`grep '- \[x\]' docs/stories` → no matches), and PROMPT.md §6 puts the demo output in the Linear closing comment. Flagged only so the closing comment actually carries "5,000 → 3 rows" and "seven scripted deltas → three rows"; the numbers are measured (`tests/events.rs:590-640`, `tests/turns.rs`).

**Accepted deliberately, verified rather than restated:** the gate case cannot show a merge. The stub's pace is `DEFAULT_STEP_DELAY = 200 ms` (`backend/stub.rs:38`) against a 25 ms window — eight windows, exactly as ADR-0031 and `docs/build.md:169-171` and `verify.sh:838-844` claim; `case_event_coalescing` asserts the contract a merge must keep and says which half it is (`verify.sh:876-895`). No scope creep: every file in the diff is HS-059, ADR-0031, the glossary entry, the story-count line, the gate case, or a test.

**Spec verdict: OK with notes** (SP1, SP2 are P3 and dismissible with reasons; no missing or partial requirement, no unasked-for behaviour).

---

## Verified by reading
- The merge predicate's four conditions and the run-ending behaviour (`events/mod.rs:181-206`) against ADR-0031's "Four boundaries, and a frame is one of them".
- Byte shape of a merged payload: `serde::Serialize` field order `message_id, kind, text` matches `Delta`'s declaration (`mod.rs:149-153`) and the only producer spelling (`backend/stub.rs:284`), and the test's exact-string assertion (`mod.rs:347`).
- No regression in the pre-existing suites that append in bulk: `tests/streams.rs:281-320` floods 3,000 `message.delta` events whose payload is `{"text":"xxx…"}` with no `message_id`/`kind`, so they are passed through untouched and `committed(...) == total` still holds; `tests/turns.rs:279` runs at `paced(30)` and `case_prompt_turn` against the 200 ms stub, both above the 25 ms window (see S10 for the coupling this creates).
- Mutation check by reading: deleting `coalesce` turns `tests/events.rs` red (`rows * 10 <= DELTAS`) and `tests/turns.rs` red (`deltas.len() < 7`); deleting either of the two identity guards turns `tests/events.rs:665` red. So the feature is not proved vacuously.
- `pub` surface: `coalesce`'s only caller is `writer.rs:61` plus the module's own tests (S3).
- ADR-0031's factual claims: stub pace 200 ms (`backend/stub.rs:38`), the repository still parsing exactly one kind (`repository.rs:21-32`), and the gate case's assertions matching `docs/build.md:167` and `verify.sh:876-895`.
- Working tree equals `65f7107`, so the artifact under review is the tree.

## Could not verify without running
- Whether `tests/turns.rs:475`/`511`/`517` actually flake: I can prove the assertion is wrong (S1) from ADR-0031's text, but the probability needs a run under load. A cheap repro: `cargo test --test turns` with the tree's other test binaries running concurrently, or temporarily inserting a `std::thread::sleep(30ms)` in `StubBackend::start_turn`'s frame loop.
- The quadratic cost in S2 as wall-clock: `cargo test --test events a_five_thousand_delta_burst` is green, so the claim is analytical (bytes processed per batch), not measured. A measurement would need a batch of one long run, e.g. 5,000 × 1 KB deltas with a 1 s commit interval.
- That `scripts/verify.sh`'s new case passes end to end (43 cases / 268 assertions is the brief's report, not mine): `scripts/verify.sh`.
- Whether the phase-2 story's acceptance #4 has been reported in Linear (SP2): that is PER-91's comment, not the repository.
- Process note for the orchestrator: `docs/review/PER-91.md` does not exist yet; PROMPT.md §4 requires the review persisted there, committed with the fixes. I have no write tool (read-only review), so this text is the artifact to persist.

## Summary
- **Standards:** 11 findings — 1 × P1 (S1, `tests/turns.rs` asserts an invariant ADR-0031 declares false), 3 × P2 (S2 quadratic re-serialisation, S3 `pub` on an internal helper, S4 shape check weaker than the ADR's "never merged"), 7 × P3. Worst: **S1**.
- **Spec:** 2 findings, both P3 (SP1 renderer/coalescer seam closed only compositionally, SP2 the reduction reported in ADR/build.md rather than the story). All four acceptance bullets implemented. Worst: **SP1**.

**Merge verdict: BLOCK** — on S1 only; it is a test-only edit (delete three batch-layout-dependent assertions, keep the rest). With S1 fixed plus S2–S4 as small edits, this is **OK with notes**.
---

## Resolution

The P1 was right and is the kind of finding this process exists for: the test that was supposed to prove
the story on the live path asserted an invariant the ADR this change ships declares **false**.

| Finding | Triage | How / why |
| --- | --- | --- |
| **S1 P1** — `tests/turns.rs`'s adjacency loop (and the `== 3` and kinds assertions) hold only because `paced(0)` happens to fit one 25 ms window; ADR-0031 says the log's adjacency is not the rule | **Fixed, by making the premise a fact** | The test now gives the writer a **two-second** commit interval (`state_with_writer`), which is longer than a whole zero-paced turn, so the whole turn is one batch: the three-row count, the kinds and the adjacency loop are then true *of that batch* rather than of the host's speed, and the comment says which half it is. The alternative the reviewer suggested (drop the three assertions) would have removed the story's only live-path proof of the merge; a deterministic window keeps it. `tests/turns.rs:279`'s pre-existing seven-delta assertion gained the same explanation (S10). |
| **S2 P2** — `coalesce` re-parsed and re-serialised the accumulated payload once per delta, so a run costs the square of its text (ADR-0018's own argument against `textContent +=`) | **Fixed** | The run is now an `Open { row, delta, deltas }` **outside** the rows: text is appended with `push_str` and the payload is serialised once, when the run closes — and a run of one is never re-serialised, so a lone delta keeps its producer's bytes. |
| **S3 P2** — `coalesce` was `pub`, widening the crate's API with one internal caller | **Fixed** | `pub(crate) fn coalesce`; the integration tests drive the writer, as the module's own visibility pattern already assumed. |
| **S4 P2** — the shape check accepted a payload that *contains* the three keys, then dropped any fourth key on merge — weaker than ADR-0031's "never merged: written exactly as the producer sent it" | **Fixed** | `parse_delta` requires an object of exactly three keys, and the malformed test gained the four-key case: `{"…","x":1}` is written byte-for-byte and ends its run. |
| **S5 P3** — `assert_eq!(events.len(), 3)` after `take(3)` cannot fail | **Fixed** | Removed; the comment now says that five arrivals are the proof (`take(5)` is what would time out). |
| **S6 P3** — two `delta` names in one file, the parser shadowed by a test helper | **Fixed** | The parser is `parse_delta` (mirrors `usage()` in the events repository). |
| **S7 P3** — the writer's failure log now counts rows, not the events it lost | **Fixed** | It logs both: `appended` (before the merge) and `rows` (after), because one row can carry a whole run's text. |
| **S8 P3** — **Coalescing** sat under "Work and results" | **Fixed** | Moved to "Events and history", after **Delta**, where **Event writer**, **Seq** and **Delta kind** live. |
| **S9 P3** — `verify.sh`'s own usage text did not mention the case it runs | **Fixed** | The header sentence now names "a turn's deltas coalesced" and the project refusals' codes. |
| **S10 P3** — two pre-existing assertions became pace-dependent without saying so | **Fixed** | Comments at `tests/turns.rs`'s seven-delta assertion and at `case_prompt_turn`'s kinds assertion name the 200 ms stub pace against the 25 ms window as the reason the numbers are what they are. |
| **S11 P3** — `assert.equal(appended(split, "m1", "thinking"), "")` holds for every input | **Fixed** | Replaced by a falsifiable pair: a thinking delta appends to `thinking` and leaves `text` empty. |
| **SP1 P3** — nothing fed the coalescer's bytes through the page's parser; the two halves of "byte-identical" were pinned separately | **Fixed** | New node test `the writer's merged payload is a frame the page parses` feeds `{"message_id":"m1","kind":"text","text":"a b"}` — the coalescer's exact output shape — through `parseFrame` and `Transcript.apply`; ADR-0031 says so. |
| **SP2 P3** — the reduction is reported in the ADR and `docs/build.md`, not in the story or Linear | **Fixed in the ship comment** | The PER-91 closing comment carries "5,000 → 3 rows" and "seven scripted deltas → three rows"; the story slice is not edited (it is the Linear body's source, per the PER-34 decision). |

Re-run after the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
(228 lib, 13 `tests/events.rs`, 10 `tests/turns.rs`), `cd web && npm test` (30) and `npx tsc --noEmit`, and
`scripts/verify.sh` — all green on a clean tree, and the demo re-run afterwards.
