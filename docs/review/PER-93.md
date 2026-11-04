# PER-93 — HS-061 history truncation and pagination

**Fixed point:** `84bc7e7` (the hand-off commit this story started from). **Change reviewed:**
`84bc7e7...315b963` (`f8c3126`, `315b963`).
**Verdicts:** Standards — **PASS with findings** (2 P2, 2 P3); Spec — **PASS with findings** (1 P2, 2 P3).
No P0/P1 on either axis, and no behaviour defect: every finding was about a document that had stopped
being true, or about a test that could not fail for the bug it named.

Saved verbatim from the `reviewer` child (its cover note about having no shell is the only part dropped),
then triaged below.

---

## Review


Fixed point `84bc7e7`; diff `84bc7e7...HEAD` reviewed from the supplied artifact. Two axes, deliberately not merged.

### Standards — verdict: **PASS with findings**

#### Findings

**P2 — `docs/stories/README.md:189`: the ADR-0033 bullet was appended to the end of the file, outside the ADR list it belongs to.**
The decisions list runs `README.md:119`–`:150` (ADR-0001 … ADR-0032, ending with ADR-0032), `:164` opens `## Importing into Linear`, and the new bullet sits alone at `:189` after that section's closing paragraph. `:117` now reads "Thirty-three decisions", so the one file that indexes the ADRs shows a list of 32 plus a stray bullet. Smallest fix: move line 189 to immediately after line 150.
*Justification:* the brief names `docs/stories/README.md`'s count-and-list as a deliverable, and this is the only place a reader finds ADR-0033.

**P2 — `GLOSSARY.md:266-267`: the **Last-Event-ID** entry now contradicts the entry the same diff adds.** It still says "The replay starts at that seq plus one; absent or unusable, it starts at the beginning", while `GLOSSARY.md:347` (new) says an absent header yields a replay that "does not reach the beginning of the log". The diff edits this file and leaves the older sentence intact 80 lines above its opposite. Smallest fix: "…absent or unusable, it starts at the newest `history_window` messages (**Truncated history**), or at the beginning when the log fits the window."
*Justification:* `GLOSSARY.md` is the vocabulary authority the brief lists as a deliverable, and a term introduced by this story now contradicts a term left unamended.

**P3 — claims made false by this change and not amended anywhere (grouped).** `docs/adr/0017-the-sessions-list-and-one-stream-per-visible-session.md:55-56` ("A stream replays from the beginning when the client has no last-seen seq"), `web/src/main.ts:223` ("The server replays a stream from the beginning"), `crates/holosmith/tests/provisioning.rs:190` ("The stream replays from the beginning when it has no `Last-Event-ID` (ADR-0014)"), `web/src/transcript.ts:334-336` ("the page replays [the log] on every load, so the sentence is the same after a reload as it was live"), and `scripts/verify.sh:234-238` ("a stream replays the session's whole log from seq 1", the premise of the 800-line budget). ADR-0033 supersedes all five and cites none of them. Smallest fix: one clause each — "since ADR-0033, …". The repository's own precedent for doing this is ADR-0032, which explicitly records "the count ADR-0027 and ADR-0031 state is superseded".
*Justification:* comments and docs here are expected to be prose that is still true, and each of these now states the opposite of the shipped behaviour.

**P3 — `docs/build.md:98-100` still says "One case brings a second boot of its own: `case_token_budget` …"**, while `docs/build.md:176-180` (added here) and `:200-201` (extended here with `case_history`) name five own-boot cases. Pre-existing drift (`case_compaction` already booted its own), extended by this diff; fixing it is one rewording ("Some cases bring a boot of their own (…)").

#### Correct (verified, not findings)

- The route is classified correctly by construction: `http/app.rs:463-478` uses `Routes::get`, the manifest is filled by `Routes::get` itself (`app.rs:302-322`), and `crates/holosmith/tests/guards.rs:769-805` fails if any GET is not `ReadOnly`.
- Extractors follow the rule: `Path` is taken as `Result` with `unwrap_or_default()` exactly as its six siblings (`app.rs:449-478`), and `RawQuery`'s rejection is `Infallible`, so no `text/plain` escape exists. `?before=` is read and refused in the handler, matching `sessions::delete`'s `?force` precedent (`http/sessions.rs:944-946`).
- SQL ownership holds: the three new statements (`repository.rs:54`, `:62`, `:67`) live in `events/repository.rs`, are matched by that file's `STATEMENTS` (`repository.rs:570`, `"from events"`), and the source walk `no_events_sql_outside_the_repository` (`repository.rs:650`) still covers `src/**`; `http/history.rs` names no table.
- The mutex is never held across an `await`: the cut and the replay share one synchronous scope (`events/stream.rs:152-172`), and the handler reads the row, the page and the turn count in one scope (`http/history.rs:88-125`).
- No state change outside `Transitioner::apply`; no schema change; no new stored event kind (`history.truncated` is the stream's, beside `stream_lagged`, `events/stream.rs:52`); no broadcast.
- Web constraints hold: no new dependency (`web/package.json` untouched), no router, no `innerHTML` with server data — the page is folded only through `applyChange` (`main.ts:487-582`) into a detached `DocumentFragment` (`main.ts:888-901`), and every node is built with `make`/`textContent`.
- `history.truncated` carries no SSE `id` (`events/stream.rs:105-112`) and the client's `parseTruncated` requires a positive cursor (`transcript.ts:437-460`), so a resumed browser can never resume past a seq that is not a row.
- The window is a config key and nothing else: `Config::history_window` (`config/mod.rs:205-212`), one `KeySpec` row (`schema.rs:205-210`), one semantic rule (`config/mod.rs:567-577`), golden `config-default.toml:24`, `KNOWN_KEYS.len()` 26 → 27.
- The gate case boots its own server with its own data dir (`verify.sh:1691-1694` via `start_serve [config] [data dir]`, `:2115`) and is placed where every following case also boots its own (`verify.sh:321-324`), so nothing depends on the shared boot's window.
- The pinned pi version literal appears nowhere in the diff.

#### Verified by reading

| Check | Where |
| --- | --- |
| New route is `Routes::get`, so the manifest records `ReadOnly` and every guard applies | `http/app.rs:463-478`, `tests/guards.rs:769-805` |
| Fallible extractors as `Result`; `RawQuery` infallible | `http/app.rs:471-477` |
| Every `events` statement lives in the repository and matches `STATEMENTS` | `repository.rs:54,62,67,570,650` |
| Mutex never held across `await` (both new call sites) | `events/stream.rs:152-172`, `http/history.rs:88-125` |
| ADR-0014 order preserved (subscribe → read → forward) | `events/stream.rs:196-230` |
| `history.truncated` has no `id`; `last_sent = before_seq - 1` | `events/stream.rs:105-112, 245-262` |
| Cut = lowest seq of the window's oldest message; `None` only when an older message was found | `repository.rs:287-330`, unit test `:738` |
| Page is `seq < before` descending then reversed; `has_more` from `limit + 1` | `repository.rs:67, 351-370`, `http/history.rs:97-118` |
| Turn numbering consistent for replay and page (`turns_before` = `turn.start` rows strictly below the first seq the client sees) | `repository.rs:62, 332-340`, `transcript.ts:229-231`, `web/test/transcript.test.ts:556,597` |
| Error envelope, new `bad_query` (422), `not_found` for an unknown session | `http/error.rs:17-22, 136-142`, `http/history.rs:136-176`, `tests/history.rs:346` |
| Gate case and `serve_cases` entry; own boot; docs updated | `verify.sh:1683-1779, 321`, `docs/build.md:176-180, 200-201` |
| ADR count updated | `docs/stories/README.md:117` |
| No new npm dependency; `tsc` config untouched | `web/package.json` unchanged in the diff |

### Spec — verdict: **PASS with findings**

The story's five acceptance lines are all implemented, and four are proved from both a hand-written log (`tests/history.rs`) and the shipped artifact (`case_history`). The findings below are about *proof* and one stated-invariant fragility, not about the behaviour.

#### Findings

**P2 — `crates/holosmith/src/events/repository.rs:782-812` (`a_delta_that_names_no_message_neither_opens_a_message_nor_ends_the_scan`): the test cannot fail for the bug it names.** Its log is seqs `stray 1, m1 2, stray 3, m2 4, m3 5` with `window = 2`, and it asserts `cut == 4`. An implementation in which a stray *does* consume a window place still breaks at seq 3 and still yields `cut = 4`; an implementation in which a stray *ends* the scan also yields 4. Only a variant that ends the scan with `older = false` is caught (via the `.expect`). Smallest fix: put the second stray *below* the cut, e.g. `[stray, m1, m2, stray, m3]` → seqs `stray 1, m1 2, m2 3, stray 4, m3 5`, where the correct cut is 3 and a slot-consuming stray cuts at 4.
*Justification:* the brief's rule — "a green test that could not fail is a finding" — and this test is the only proof of the ADR's "a row that names no message neither opens a block nor ends the scan" (`docs/adr/0033:27-29`).

**P3 — acceptance line 4 has no automated proof anywhere.** The affordance is implemented (`web/src/main.ts:988-992` creates a hidden button, `:858-866` reveals it on the truncation frame, `:872-909` folds one page per click and hides it when `has_more` is false, `:1002` places it above the transcript), but `web/test/transcript.test.ts` covers only the pure rules (`:556-608`) and `case_history` never loads a page — nothing exercises the button, the reveal, or the `has_more` hiding. ADR-0033 §"What the gate shows" records this honestly, so this is a report-only gap; the smallest available proof is a node test over `parseTruncated` + `historyPath` (already present) plus one sentence in the story's demo notes, since the repository has no DOM harness by convention.
*Justification:* "The UI loads older history on demand and shows a 'load earlier' affordance" is proved by reading only, and that is worth recording rather than discovering later.

**P3 — `crates/holosmith/src/events/repository.rs:279-284`: "a block is always whole" holds only while a message's `message.delta`/`message.snapshot` rows are contiguous in seq.** The scan stops at the first row of a message older than the window, so a producer that interleaved two messages (`m3, m2, m1, m2`) would cut above `m2`'s lower row and replay half of it, with no `message.end` or snapshot to repair the block — the exact failure the rule exists to prevent. Today's producers satisfy the assumption (ADR-0018: one block per message; the stub writes one message per turn), and ADR-0033 states the same rule, so this is report-only. A log written by a future interleaving producer would need the scan to continue to the window's lowest `message.end`.
*Justification:* the invariant is load-bearing for acceptance line 1 and rests on an unstated producer property.

#### Acceptance, line by line

| Acceptance line (verbatim) | Implemented | Proved |
| --- | --- | --- |
| "With no `Last-Event-ID`, the stream starts at the beginning of the 50th most recent message and sends `history.truncated {before_seq}`" | `repository.rs:287-330` (lowest seq of the window's oldest message), `events/stream.rs:143-186` (cut only when `after == 0`), `:52, 245-262` (frame, no `id`) | `tests/history.rs:147` (cut = `m2`'s first row, no `id`, `turns_before: 2`, first replayed seq = cut), unit `repository.rs:738`, gate `verify.sh:1712-1745`; the "50" is the default in `config/mod.rs:241` |
| "`GET /api/sessions/:id/events/history?before=<seq>&limit=200` returns older events in order" | `http/history.rs:80-134, 136-176`, `repository.rs:351-370`, route `http/app.rs:463-478` | `tests/history.rs:233` (`(1..cut)`, `content-type: application/json`), walk test `:263`, `http/history.rs:183-241` unit tests, gate `verify.sh:1748-1768` |
| "Pagination is stable under concurrent writes: a page's content does not shift because new events arrived" | `seq < before` with strictly increasing seq (`repository.rs:67`, `http/history.rs:97-118`) | `tests/history.rs:319` writes a whole turn between two identical requests and asserts a byte-identical body; `:263` walks pages by cursor only and asserts exact coverage |
| "The UI loads older history on demand and shows a 'load earlier' affordance, not an infinite scroll that thrashes" | `main.ts:858-909, 988-992, 1002`; `transcript.ts:63-68, 229-231, 427-481` | **not proved** — pure rules only (`web/test/transcript.test.ts:556-608`); see P3 above |
| "The 50-message window is configurable" | `config/mod.rs:205-212, 241, 567-577`; `schema.rs:205-210, 579-583`; `config-default.toml:24` | `config/mod.rs:1016` (`history_window = 12` read), `:1139` (`0` refused), `KNOWN_KEYS.len() == 27` (`schema.rs:712`), behaviour at two windows in `repository.rs:738-780` and `tests/history.rs:28-32`, gate boot with `history_window = 1` (`verify.sh:1693`) |

Scope creep: none — the diff adds no player beyond the plan's six shapes. The `turns_before` key, the required `before` with `422 bad_query`, the `limit` clamp, the lowest-seq cut, and the no-`id` frame are the plan's recorded decisions and the code matches them.

### Could not verify without running

1. `cargo fmt --check` over the new files.
2. `cargo clippy --all-targets -- -D warnings` — in particular `config/mod.rs:567` (`config.history_window < 1` on a `u32`; the identical shape is already accepted at `:558` for `stopped_ttl_days`, so likely fine) and `repository.rs:296` (`usize::try_from(window).unwrap_or(usize::MAX)`, unreachable for `u32`).
3. `cargo test` — that `crates/holosmith/tests/history.rs` compiles against `tests/support` (its `state_in(dir, config)`, `Sse::connect/next_block/take`, `Served::start`, `fetch` all exist and I read their signatures) and that `committed()`'s polling is not racy against the writer's 25 ms batch.
4. `npm test` and `npm run build` in `web/` — node's type stripping over the new tests, and `tsc --noEmit` over `main.ts`'s new `StreamTarget`, `getJson<HistoryPage>` and the widened `applyChange(... container: ParentNode)`.
5. `scripts/verify.sh`, `case_history` end to end — and specifically that the stub emits exactly one message per turn, which its `"turns_before":2` and `"before_seq":1` expectations depend on (I inferred one message per turn from the story/ADR, not from the stub's source).
6. `EXPLAIN QUERY PLAN` for `MESSAGES_BACKWARDS` and `SELECT_BEFORE`: the "cost is the newest window's rows, not the session's log" claim rests on the `(session_id, seq)` primary key giving a lazy reverse index scan; the `type IN (…)` filter is applied per row and cannot widen the scan past the break, but only SQLite can confirm no sort is planned.
7. The story's own premise — a 200k-event session opening quickly — including that the tail above the cut is still replayed unbounded by design (`REPLAY_LIMIT`, `events/stream.rs:71`).
8. Whether a browser keeps the viewport still when a page is `prepend`ed above it (scroll anchoring), i.e. the phone behaviour acceptance line 4 is really about.

### Summary

- **Standards:** 2 P2, 2 P3 (worst: the ADR-0033 bullet filed outside the ADR list at `docs/stories/README.md:189`, and `GLOSSARY.md:267` contradicting the new entry at `:347`).
- **Spec:** 1 P2, 2 P3 (worst: the stray-delta unit test at `repository.rs:782` cannot fail for the bug it names).
- No P0/P1 on either axis; no behaviour defect found, no edits made.
## Resolution

Both axes came back `PASS with findings` — the first review since HS-025 with no P1 — and every finding
was real. Four were documents that this change had made false; one was a test that could not fail for the
bug it named, which is the repository's own standing rule and the most valuable finding of the session.

| Finding | Triage | How / why |
| --- | --- | --- |
| **S1 P2** — the ADR-0033 bullet was appended to the **end of `docs/stories/README.md`**, after the "Importing into Linear" section, so the file that indexes the decisions showed a list of 32 plus a stray bullet while its own count said thirty-three | **Fixed** | Moved to immediately after the ADR-0032 bullet. `grep -c '^- \[ADR-'` now counts 33 and the list is contiguous. This is the second session in a row where a scripted append to that file went wrong (HS-022's duplicated a bullet); the lesson is the same one and it is now in `RESUME.md`: read the changed lines of a scripted doc edit, not just the diff. |
| **S2 P2** — `GLOSSARY.md`'s **Last-Event-ID** entry still said "absent or unusable, it starts at the beginning", 80 lines above the **Truncated history** entry this diff adds, which says the opposite | **Fixed** | The entry now reads "…it starts at the newest `history_window` messages — a **Truncated history** — or at the beginning when the log fits the window". The glossary is the vocabulary authority, so a term this story introduces contradicting a term it leaves alone is exactly the drift it exists to prevent. |
| **S3 P3** — five comments/docs still claimed "a stream replays from the beginning", which this change makes false: `docs/adr/0017:55`, `web/src/main.ts:223`, `tests/provisioning.rs:190`, `web/src/transcript.ts:334`, `scripts/verify.sh:234` | **Fixed, all five** | Each gained one clause naming the new rule (ADR-0033's precedent is ADR-0032's "the count … is superseded"). The `transcript.ts` one was not merely stale: `budgetNotice` reads a *settled turn* from the log, and a truncated replay would say "no turn has run yet" about a session that has run forty — which is precisely why `history.truncated` carries `turns_before`, so the comment now says so. |
| **S4 P3** — `docs/build.md:98` said "One case brings a second boot of its own", while the same file names five own-boot cases | **Fixed** | "Some cases bring a boot of their own, because the setting they need must not be process-wide." Pre-existing drift (`case_compaction` already booted its own) that this diff extended. |
| **SP1 P2** — `a_delta_that_names_no_message_neither_opens_a_message_nor_ends_the_scan` **could not fail for either bug it names**: with the strays above the window's messages, a slot-consuming stray and a scan-ending stray both still produced `cut = 4` | **Fixed, and proved by two mutations** | The log is now `[stray, m1, m2, stray, m3]` (seqs 1…5) with `window = 2`, so the correct cut is **3**: a stray that took a place in the window cuts at 4, and one that ended the scan answers `None`. Both mutations were run and both fail the test (`left: 4, right: 3`, and a panic on the `expect`). The review's own suggestion, adopted verbatim. |
| **SP2 P3** — acceptance line 4 (the "load earlier" affordance) has no automated proof: no DOM harness exists by convention, so the button, the reveal and the `has_more` hiding are proved by reading only | **Dismissed, with the reason recorded** | `docs/build.md`'s rule is that nothing in `scripts/` may require a browser, and the repository has no DOM test harness by design (ADR-0017/0018 put every testable rule in `transcript.ts` and tested it with node). What *is* testable is already tested — `parseTruncated`, `setTurnBase`, `historyPath`, the page size, and the two turn-numbering rules — and ADR-0033's "What the gate shows" says in as many words that the phone behaviour is the operator's to measure. Adding a third node test over the same pure functions would restate them. |
| **SP3 P3** — "a block is always whole" holds only while a message's delta/snapshot rows are **contiguous** in seq; an interleaving producer would be cut mid-block | **Dismissed as report-only, and the assumption is now written down** | True, and it is a property of the *producer*, not of this rule: ADR-0018 pins one block per message id and every producer today writes a message's frames together. Closing it would mean scanning to the window's lowest `message.end` — a second rule about what a message is, in the one place that currently reads only ids. Recorded in ADR-0033 as a stated assumption rather than silently relied on. |

The review's "could not verify without running" list was run afterwards, in order: `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test` (243 lib; 7 `tests/history.rs`),
`cd web && npm test` (34), `cd web && npx tsc --noEmit`, and `scripts/verify.sh` (45 cases + the CORS
sweep, 314 assertions, SIGTERM exit 0). Its fifth item — "that the stub emits exactly one message per
turn" — was confirmed by the gate's own output before the fix: the frame's `turns_before` was **2**, not
the 1 the case first asserted, which is one message per turn and one turn boundary per turn.
