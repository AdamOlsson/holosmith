# Review — PER-46 (HS-014, session screen with a live transcript)

Fixed point: `769ae55`. Diff: `769ae55...HEAD` plus the four new files. Reviewer ran
read-only with no shell and checked its claims against `axum-0.8.9`'s extractor source, the
ADRs and the story slice.

## Review

**Verification basis.** Read-only role, no shell: I read the exported diff (`/tmp/holosmith/reviews/PER-46-diff.txt`), the fixed point's tree, the spec/story/ADR sources the brief names, `axum-0.8.9`'s own `Path` extractor in the toolchain registry, and `scripts/verify.sh`'s helpers. I ran nothing — no `cargo`, no `verify.sh`, no `npm test`, no browser.

---

# Standards axis

### Correct (evidence)

- **The new route is genuinely read-only and guarded.** `app.rs:332` registers it through `Routes::get`, so `register(path, Method::GET, RouteKind::ReadOnly, …)` (`app.rs:113-118`) fills the manifest, and `edge` (`app.rs:358-380`) runs `guards.check` on it like every non-exempt request. `tests/session_screen.rs:172-182` reads the manifest entry, and `:126-144` proves `403 bad_host` with a foreign `Host`. The handler reads under the one lock and does not await inside it (`sessions.rs` `one_locked`), the same shape as `list_locked`.
- **No path traversal of markup.** Every node in the web change is built with `createElement`/`createTextNode` and filled with `textContent` (`main.ts:113-124`, `:429`, `:435-437`, `:573`, `:693`); `grep` for `innerHTML|insertAdjacentHTML|outerHTML|document.write` over `web/src` finds nothing. The one new attribute write is `head.href = \`/sessions/${session.id}\`` (`main.ts:170`) — a `path`-prefixed property assignment, not markup, and the id is server-minted 16 hex (`sessions/mod.rs:87`). `parseFrame` decodes JSON and never produces markup.
- **The 10k rule is enforced structurally, not by prose.** `applyChange` looks blocks up in a `Map` and never queries the document; a delta is one `appendChild(createTextNode(...))` (`main.ts:425-431`); there is no `textContent +=` anywhere. `web/test/transcript.test.ts`'s `10k deltas are 10k appends` asserts one block, 10 000 appends, zero replacements.
- **The gate rule and the ADR rule are both honoured.** Two `case_*` added and listed (`verify.sh:280-281`, functions at `:637-689`); `docs/stories/README.md:117` reads "Eighteen", the bullet list has 18 entries, and `docs/adr/0018-…md` exists, so the new link resolves. `package.json` gains one script and `package-lock.json` is untouched; no dependency arrived.
- **Assertion count checks out statically.** 7 new assertions in `case_session_detail` (2 status/header + 2 `case` branches + 3 for the 404 envelope), 2 in `case_session_screen`, plus 3 new saved `.http` files for the CORS sweep → 66 + 12 = **78**, and 23 entries in `serve_cases`. `created_id` is set by `case_create_session` (`verify.sh:461`), which runs earlier in the array.

### Findings

**S1 · P1 · `web/src/main.ts:483-516` — the composer's Prompt button performs a native form submit, so pressing it reloads the screen instead of naming HS-015.**

```ts
493:   const prompt = make("button", "", "Prompt");
494:   prompt.type = "submit";
...
515:   form.append(label, actions);
516:   section.append(form, notice);
```

`composerSection` never registers a `submit` listener, and the form has no `action`, so clicking Prompt (or, later, Enter in a single-line field) fires the HTML default: a GET navigation to `/sessions/<id>?guidance=…`, i.e. a full page reload. The `say("Prompting arrives with HS-015; nothing was sent.")` notice at `:511` is painted and then discarded by the navigation, and the transcript is re-fetched from seq 0. This is the exact failure the story's substitute affordance exists to avoid, and it is *not* covered by the "DOM half is unverified" limitation — it is default HTML behaviour, provable from source. The create form shows the intended pattern 130 lines above: `form.addEventListener("submit", (event) => { event.preventDefault(); … })` (`main.ts:364-365`).

*Smallest fix:* one line in `composerSection` — `form.addEventListener("submit", (event) => event.preventDefault());` (keeps `type="submit"` so HS-015 inherits a working form), or `prompt.type = "button"` for now.

**S2 · P1 · `crates/holosmith/src/http/app.rs:332` — the new route turns `GET /api/sessions/%FF` from ADR-0008's envelope into axum's `text/plain` 400.**

```rust
332:         .get("/api/sessions/{id}", move |Path(id): Path<String>| {
```

A percent-encoded segment that is not valid UTF-8 makes `Path<String>` reject: `axum-0.8.9/src/extract/path/mod.rs:169-175` builds `ErrorKind::InvalidUtf8InPathParam` → `StatusCode::BAD_REQUEST` (`:438-446`) → `(status, body_text).into_response()` (`:453-462`), i.e. `400 text/plain` "Invalid URL: Invalid UTF-8 in `id`" with no `{"code","message"}`. Before this route that path fell through to `fallback` (`app.rs:406-424`), which answers `ApiError::not_found()` for anything under `/api/`. So this diff *introduces* the break on that URL; the events route (`app.rs:350`) already has the same shape, which is why it has never been noticed.

The codebase names this class explicitly ten lines below (`app.rs:336-338`: "a rejection left to axum answers a `text/plain` 400/413 outside ADR-0008's envelope, so the handler maps it to `bad_body` itself").

*Smallest fix:* take `Result<Path<String>, PathRejection>` (or `RawPathParams`) and map the rejection to `ApiError::not_found()`; one change covers both this route and `{id}/events`.

**S3 · P2 · `docs/adr/0018-…md` — no `## Consequences` section, so the ADR never records its own evidence.**

Every ADR from 0006 to 0017 ends with `## Consequences` (e.g. `0017-…md:140`), and that section is where the gate case and the socket test are named (`0017-…md:142-148`). ADR-0018's headings are `:10`, `:42`, `:62`, `:82` and it ends at "The next story's demo is the first that can look at it." Neither `case_session_detail`, `case_session_screen` nor `crates/holosmith/tests/session_screen.rs` appears anywhere in it.

*Smallest fix:* add `## Consequences` with three bullets (the two cases and their assertions, the new socket test with its five tests, and "`GET /api/sessions/{id}` answers the bare row, so it carries `client_key` exactly as the list does").

**S4 · P2 · `docs/adr/0018-…md:77` — the ADR maps the composer's buttons to the wrong stories.**

```
77: `error` or `deleting`. The endpoints are HS-015's, HS-016's and HS-017's, so pressing one names the
78: story that will wire it rather than sending a request that would 404.
```

The sentence follows "prompt on `idle` … steer and abort on `running`" (`:75-77`), so in order it reads prompt→HS-015, steer→HS-016, abort→HS-017. The story file says otherwise — `phase-1-session-lifecycle-stub.md:165` is "HS-016 — Abort with the full ladder", `:182` is "HS-017 — Steer a running turn" — and the code and the button notices get it right (`main.ts:511-513`: steer→HS-017, abort→HS-016; comment `:475-477`). HS-015's implementer reading the ADR will look for steer in the wrong story.

*Smallest fix:* name them — "prompt is HS-015's, abort HS-016's, steer HS-017's".

**S5 · P2 · `docs/adr/0018-…md:23-27` — "The names come from spec item 15 verbatim" is true for two of the four kinds.**

```
23: transcript. The names come from spec item 15 verbatim: it names `message.snapshot` holding "full
24: text, thinking, message id", and its coalescing rule is "consecutive deltas of the same kind and
25: message".
```

Both quotations are accurate (`HolosmithSpec.md:256-257`), but item 15 never spells `message.delta`, `turn.start` or `turn.end`; the spec's other event names are `message.end` (`:245`) and `tool.end` (`:259`), and the only source for explicit turn events is HS-015's acceptance bullet ("Turn start and end are explicit events", story `:160`). Since HS-015 is bound by this ADR, it should say which two names are the spec's and that the boundary names are this story's invention (pinned here so HS-015 cannot pick another).

**S6 · P2 · `web/src/main.ts:435-437` — a snapshot that empties a channel leaves it visible, contradicting `:408`.**

```
408:   // An empty channel is not shown at all, so a text-only message has no blank "thinking" band.
...
435: function replaceChannel(node: HTMLElement, value: string): void {
436:   node.replaceChildren();
437:   appendPiece(node, value);
```

`appendPiece` returns early on `""` (`:426-428`) and only ever sets `hidden = false` (`:430`). So for a block whose `thinking` channel already had deltas (`hidden === false`), a text-only `message.snapshot` empties it and leaves `hidden === false`: an empty `.thinking` div, which `style.css` still gives `margin-bottom: 0.25rem`. Cosmetic, but the comment promises otherwise, and it is exactly the case ADR-0018:34-36 and the test `a snapshot frame carries the whole message, and an absent channel is empty` describe.

*Smallest fix:* `node.hidden = value === "";` after `replaceChildren()` in `replaceChannel` (or set `hidden` from the append).

**S7 · P2 · vocabulary drift — `GLOSSARY.md:209-211` tells readers to avoid "channel", and this same diff introduces it as a domain word.**

```
209: **Delta kind**:
210: Which of a message's two channels a delta belongs to: … one message holds both channels.
211: _Avoid_: Channel, stream, type, part
```

The glossary is the vocabulary authority (`RESUME.md:474`), and the new code and ADR use the avoided word for the same concept: `transcript.ts:18-19` ("The two channels a message's output arrives on"), `main.ts:404` and `:423` (`channelNode`, `replaceChannel`), `ADR-0018:36` ("once a block has two channels"). One of the two should move — the cheapest is one clause in the glossary entry: "a block's two channels are the nodes its kinds write into (`.text`, `.thinking`)", which is true and keeps the identifiers.

**S8 · P2 (`docs/build.md:206-210`) — the node floor for `npm test` understates what node needs.**

```
206: **node and npm** for the app shell: …
209:   build. Vite 7 needs node 20.19 or later; `npm test` is node's own runner over the
210:   transcript's rules in `web/test/` (ADR-0018), so node 22 or later is what those tests want
```

`node --test test/*.test.ts` depends on native type stripping, which is on by default only from node 22.18 and 23.6 (before that it needs `--experimental-strip-types`, and below 22.6 it is unsupported). "node 22 or later" therefore admits 22.0-22.17, where the gate fails with an unknown-extension error — the same class of doc/prerequisite mismatch PER-35 S1 caught. `ADR-0018` is accurate ("Node 24 strips TypeScript types natively"); the build doc should say 22.18+ (or just drop the sentence and keep Vite's 20.19 floor plus the note that the transcript tests want the stripping default). Unverifiable here without an old node, hence P2.

**S9 · nit — `GLOSSARY.md:82` vs `GLOSSARY.md:190`: one route, two placeholder spellings.**

```
82:  … Its first answer is `GET /api/sessions/{id}` and everything after that arrives on the session's stream …
190: The HTTP response at `GET /api/sessions/:id/events`: one session's committed events …
```

The two entries sit in the same section and disagree on whether the variable is `{id}` (ADR-0014/0018, the axum spelling) or `:id` (the spec's). Pick one for the glossary.

**S10 · nit — "composer" is used as vocabulary but has no glossary entry.** `GLOSSARY.md:82` defines the session screen as containing "the composer"; `main.ts`, `style.css` and ADR-0018 all name it; every sibling term this story introduced got an entry (Session screen, Turn boundary, Delta kind, Message id, Transcript). One line would keep the set complete.

**S11 · nit — `web/index.html:12-14` is stale for the second screen.** `<p>Loading this machine's sessions…</p>` and `<noscript>… list your sessions and start a new one.</noscript>` are the whole first-paint text for `/sessions/<id>` too. PER-45 refreshed this line for the list; the shell now serves two screens. (No gate impact: `case_app_shell`/`case_session_screen` compare the file to itself.)

**S12 · P2, report-only — `RESUME.md` is now wrong in five places.** `:196` "21 cases plus a CORS sweep (66 assertions)", `:471` "The current gate is 21 cases / 66 assertions", `:540` same, `:473` "(now seventeen)", `:255` "docs/adr/0001…0017", and `:269` "Not built yet: `GET /api/sessions/:id`, the session screen (HS-014)". It is a cold-start snapshot refreshed by a handoff commit (the fixed point is exactly such a commit), not a per-story deliverable, so this may be deliberate — but a cold start reading `:269` today would plan the endpoint twice.

**Standards verdict: BLOCK on one small fix (S1), plus S2 before release.** Both are one-liners; nothing else is a code defect. After those, Standards is OK with notes (S3-S12 are doc/vocabulary consistency).

---

# Spec axis

### Bullet 1 — subscribes and appends deltas per message id; thinking visually distinct — **satisfied in the rules**

`main.ts:542-578` opens one `EventSource` at `streamPath(id)` and subscribes to exactly `TRANSCRIPT_EVENTS`; `transcript.ts:126-165` (`Transcript::apply`) creates a block on first sight of an id and emits `delta` changes naming the channel; `main.ts:459-470` appends into `nodes[change.delta]`. Distinctness is CSS, not markup: `.thinking` is italic, 0.875 rem, 0.7 opacity, `.text` is `pre-wrap` (`style.css`). The DOM half is unverified, as the brief says.

### Bullet 2 — reconnect by `Last-Event-ID`; a snapshot replaces by id, safe mid-delta — **satisfied, with one asymmetry worth fixing**

The page invents no cursor (`transcript.ts:174-176` `streamPath`, ADR-0018:71-73), which is right — the browser resends the last `id:` (`streams.rs:19-21`, ADR-0014:148-150). Replacement is `replaceChildren` on both channels, and `a snapshot replaces by message id, safe after partial deltas` pins it.

- **P2 — `web/src/transcript.ts:89-91` treats an absent `text` as "drop the frame", while `ADR-0018:34-36` says an absent channel is the empty string.**
  ```
  89:   const full = text(fields["text"]);
  90:   if (full === null) {
  91:     return null;
  ```
  ```
  ADR-0018:34: - **An absent snapshot channel is the empty string, not "leave it".** A snapshot is the message's
  ```
  Only `thinking` gets the `?? ""` treatment (`:99`). A producer emitting `{"message_id":"m","thinking":"x"}` has the whole frame dropped silently, so the old thinking deltas stay on screen — the exact "stale half" the ADR rule forbids. The tests pin `{"message_id":"m1"}` → `null`, so the behaviour is deliberate for both-absent, but the one-channel case is undefined by both. *Smallest fix:* `const full = text(fields["text"]) ?? ""` (keeping the id requirement), or state in ADR-0018 that `text` is mandatory on a snapshot.
- **note — the ADR never says whether a delta may follow a snapshot for the same id.** The client appends onto the reconstructed text, which is only correct if the producer stops. The spec bounds this (`HolosmithSpec.md:257`: snapshots come from compaction of messages completed >24 h ago), so no order the server can produce today breaks the rule — but HS-015 is the story that could, and one sentence in ADR-0018 ("a snapshot is the last frame for its id") would close it. I found no ordering the current server can produce that breaks append-or-replace: replay is seq-ordered (`events/stream.rs`), `stream_lagged` closes and reconnects after the last real seq (ADR-0014:97-100), and the `Transcript` model survives the reconnect because it is created once outside the listener (`main.ts:548`).

### Bullet 3 — state badge, reason, per-turn boundary visible — **satisfied**

Badge from the answer with the reason folded in (`main.ts:593`, `stateText`), patched only on a real transition (`:571-575`, ignoring the `from === to` step reports as ADR-0016 requires); the boundary is a `.turn-boundary` div per `turn.start`/`turn.end` (`main.ts:447-455`).
- **nit** — a `turn.end` seen with no prior `turn.start` renders "turn 0 settled" (`transcript.ts:141-146` starts `turn` at 0 and only increments on `start`). Unreachable today, reachable once HS-061 truncates a replay to the last 50 messages; the glossary records the counting rule (`GLOSSARY.md:89-91`), so this is only a string choice ("a turn settled" when the number is 0).

### Bullet 4 — composer exists with Steer/Abort only when `running` — **affordance matrix correct, the button is broken (S1)**

`affordances` (`transcript.ts:170-176`) matches spec item 8's table exactly: prompt only on `idle` (row "idle | prompt | running"), steer/abort only on `running` (rows "running | agent\_settled/abort completes/abort ladder"), nothing on `starting`, `stopped`, `error`, `deleting`; `affordances("running")`/`("idle")`/the three quiet states are all asserted. The wiring defect is S1 above: the one affordance pressable in `idle` submits the form.

### Bullet 5 — responsive at 10k events, no full re-render per delta — **satisfied**

One appended text node per delta, `Map` lookup, no document query per frame, and the model-level test proves one block / 10 000 appends / zero replacements. The DOM cost itself remains unverified (stated).

### Scope and spec fit

In scope, minimal, no dependency, no `Cargo.toml`/`package-lock.json` change, no pi version spelled in any tracked doc (the rule the brief names). Item 15's history/paging (`history.truncated`, `…/events/history`) and tool output are correctly deferred (ADR-0018:56-60), and `usage`/`tool.*` frames are dropped by design (`transcript.ts:71-79`). Item 5 (25 ms batching) is respected by not inventing a client cursor.

**Spec verdict: OK with notes.** All five acceptance bullets are covered by something in this diff, none by prose alone; the two code notes are the snapshot `text` asymmetry (P2) and the "turn 0" string (nit), and the one real defect is S1, which is a Standards-axis finding because it is wiring, not contract.

---

## Tests a supervisor must run (this role has no shell)

`. ~/.pi/toolchains/env.sh && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && scripts/verify.sh` (expect 23 cases / 78 assertions), plus `cd web && npm test`. The composer fix in S1 changes no assertion; S2 changes no existing case, but a new `case_` assertion (`GET /api/sessions/%FF` → `404 not_found`) would pin it.

## Merge verdict

- **Standards: BLOCK** — S1 (composer submits and reloads the screen) must land, and S2 (envelope break on a malformed id) should land in the same pass; both are one-line fixes. Everything else is OK with notes.
- **Spec: OK with notes** — every acceptance bullet is implemented; the only code-level note is the snapshot `text` asymmetry in `transcript.ts:89-91`, which is a silent-drop path rather than a rendering error.
## Resolution

Every finding was triaged. Fixed means the change is in this commit; nothing was dismissed.

| Finding | Verdict | How |
| --- | --- | --- |
| S1 · P1 · prompt button's native submit reloads the screen | **fixed** | `composerSection` now registers a `submit` listener that calls `preventDefault`, beside the three button notices. `type="submit"` stays, so HS-015 inherits a form that submits nowhere rather than a page that forgets what it was showing. Not observable from source alone by a test here (no browser), so the ADR records the reason. |
| S2 · P1 · `GET /api/sessions/%FF` answered axum's `text/plain` 400 | **fixed** | Both `{id}` routes take `Result<Path<String>, PathRejection>`; a rejected segment becomes the empty id, which no row holds, so the answer is ADR-0008's `not_found`. Pinned twice: `tests/session_screen.rs::a_segment_that_is_not_utf8_is_the_envelope_not_a_plain_text_400` (both routes) and `case_session_detail`'s malformed fetch in `scripts/verify.sh`. |
| S3 · P2 · ADR-0018 had no `## Consequences` | **fixed** | Added: the bare-row answer and its `not_found` rule, the six-test socket file, the two `verify.sh` cases with the 21/66 → 23/82 count, and the twelve node tests with where they run. |
| S4 · P2 · ADR mapped the composer's buttons to the wrong stories | **fixed** | Now named explicitly: prompt is HS-015's, abort HS-016's, steer HS-017's — the order the story slices use. |
| S5 · P2 · "the names come from spec item 15 verbatim" overclaimed | **fixed** | Two names are quoted from item 15 (`message.snapshot`, the coalescing rule); `message.delta` is this story's name for the piece item 15 calls a delta, and `turn.start`/`turn.end` are this story's, pinned so HS-015 cannot pick a second spelling. |
| S6 · P2 · a snapshot that empties a kind left its node visible | **fixed** | `replaceChannel` (now `replaceKind`) sets `hidden = value === ""` after `replaceChildren`, so a cleared thinking band leaves no gap. |
| S7 · P2 · "channel" used as a domain word while the glossary avoids it | **fixed on both sides** | The glossary is the authority, so the code moved: `kindNode`, `replaceKind`, and the comments now say "kind's node". The glossary's `Delta kind` prose says "two kinds of output" and keeps `Channel` in its `_Avoid_` list. |
| S8 · P2 · `docs/build.md` said "node 22 or later" for `npm test` | **fixed** | **22.18 or later**, with the flag history (22.6–22.17 need `--experimental-strip-types`), because the transcript test imports TypeScript and depends on native type stripping. |
| S9 · nit · `{id}` in the new glossary entry vs `:id` in `Stream` | **fixed** | The glossary entry now uses the spec's `:id`, matching `Stream` and the vocabulary authority's spelling. The ADRs keep `{id}`, the axum spelling, as they already did. |
| S10 · nit · "composer" had no glossary entry | **fixed** | Added **Composer**, with the three actions and the state that offers each, and `_Avoid_: Input bar, message box, chat box, prompt bar`. |
| S11 · nit · `web/index.html`'s placeholder named only the list | **fixed** | "Loading…" for the shell, and the `noscript` now names the transcript too. `case_app_shell`/`case_session_screen` compare the file to itself, so nothing else moved. |
| S12 · P2 · `RESUME.md` was stale in five places | **fixed** | Refreshed for HS-014 in a follow-up docs commit: HEAD and the HS-014 row, the Done/next-issue counts, the gate's 23 cases / 82 assertions, the tree (transcript model and its tests, `sessions::one`, `tests/session_screen.rs`), ADR count eighteen, and HS-015's own two facts. |
| Spec note · snapshot `text` absent was dropped while the ADR said "empty string" | **fixed** | `parseFrame` defaults **both** kinds to `""`, so a producer that omits a kind clears it instead of having its frame dropped; only `message_id` stays mandatory. The test that pinned the drop now pins the empty message. |
| Spec note · a `turn.end` with no `turn.start` rendered "turn 0 settled" | **fixed** | Renders "a turn settled" when no start has been seen — unreachable today, reachable once HS-061 truncates a replay. |
| Spec note · the ADR never says whether a delta may follow a snapshot | **fixed** | Recorded as a rule: a snapshot is the last frame for its id. Nothing the server produces today violates it (item 15's snapshots replace messages completed >24 h ago); HS-015 and HS-060 are the stories that must keep it. |
| Own note · the screen keeps its stream open in every state | recorded | ADR-0018 now states the divergence from ADR-0017's row rule and why: a row is a summary of something else, while the screen *is* the session the operator opened, and a `stopped` session's history is what they came to read. |

Gates re-run after the fixes, alone on a clean tree: `cargo fmt --check`, `cargo clippy --all-targets --
-D warnings`, `cargo test` (204 lib + 16 integration binaries, `session_screen` now six tests),
`cd web && npm test` (12 tests), `scripts/verify.sh` (23 cases + the CORS sweep, **82 assertions** — the
malformed-segment fetch added four).
