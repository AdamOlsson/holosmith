# Review — PER-65 / HS-033 (RPC mapping: tool calls and output files)

Saved verbatim from a reviewer child (`~/.agents/skills/code-review/SKILL.md`, two axes) against
`7ffb294...HEAD` (`9303ea3`) plus untracked files, with a `## Resolution` table appended below after the fixes.
Its "could not verify without running" list is what the orchestrator then ran.

## Review

**Scope.** Read the diff artifact `/tmp/holosmith/reviews/PER-65-diff.txt` (7ffb294..9303ea3) in full, then every file it touches, plus ADR-0041/0019/0032/0040, `HolosmithSpec.md` item 15, HS-033's slice and its phase preface, the plan, `docs/build.md`'s gates, `web/package.json`, `scripts/build-web.sh`. The artifact and the working tree agree on every hunk I re-read. No edits made (review-only); nothing marked "Fixed".

**Verdict:** the story is implemented and its seams are right (mapping stays I/O-free, allow-list before any join, row before any directory, additive keys, live *and* replayed rows both render). No BLOCK. Two P1s, both documentation-or-test integrity rather than behaviour: ADR-0041's answer to the story's "exit status" rests on statements the code contradicts, and the single test that owns the `session.log` bullet cannot fail.

---

## Standards

**P1 · `crates/holosmith/tests/rpc.rs:503-515`** — the test that owns acceptance bullet 5 ("`session.log` holds stderr only") cannot fail. Its only assertion is inside `if let Ok(text) = std::fs::read_to_string(&session_log)`, and nothing in that test creates the file: `Log::new` (`tests/rpc.rs:281-300`) builds a database, a writer and a session and no `SessionLog`; `Agent::launch` hands stderr back through `Ended` (`src/agent.rs:418-424`); the file is written only by `SessionLog::append`, called from the Docker backend (`src/backend/docker.rs:393`). So the guard is skipped on every run, while the plan claims "a test asserts a tool turn's text is *absent* from `session.log` … item 15's rule turned into a failure that can happen". *Fix:* write one line through `exit::SessionLog` at the top of the test, assert the file exists, then assert the tool's text is absent.

**P2 · `tests/rpc.rs:496-501`** — `the_pump_files_a_tool_output_so_the_log_carries_only_a_preview` never proves a preview: the fixture's output is 21 bytes, so `assert_eq!(tool_end["output"], "hello from the tool\n")` passes whether `ToolEnd::frame` previews or not. The only large-output preview assertion (`tests/rpc.rs:584-590`) goes through `mapped()`/`Mapped::into_frame()`, i.e. the store-less replay, so `Stored.preview` on the served path (`src/rpc.rs:161-164`) is asserted nowhere. *Fix:* give the pump test a 4096-character output and assert the log's `output` is `PREVIEW_MAX + 1` chars while the file holds all 4096.

**P2 · `tests/tool_output.rs:122-166` and `scripts/verify.sh:1362-1408`** — both traversal checks pass with `valid_id` deleted, because the canary is not where a naive join lands. The test plants `<data>/secret.txt` (`tests/tool_output.rs:132`), while `..%2Fsecret` from `<data>/sessions/<id>/tool/` resolves to `<data>/sessions/<id>/secret`; the gate looks for `SQLite` in the answer to `..%2F..%2Fapp.db`, which resolves to `<data>/sessions/app.db` while the database is `<data>/app.db` (three levels up). So `!answer.body.contains("not yours")` and the `*SQLite*` branch can never trigger. The property is still covered by unit tests (`src/tool.rs:270-330`), but the HTTP-level negative test proves nothing. *Fix:* plant the canary at `<data>/sessions/<id>/secret.txt` and use `..%2Fsecret.txt`; in the gate use `..%2F..%2F..%2Fapp.db`.

**P2 · `docs/adr/0019-the-turn-runner-and-the-backends-events.md:61-69`** — the event contract is not amended in place, though ADR-0041 claims ADR-0040's pattern (`docs/adr/0041-…:36`). ADR-0019 still pins `tool.end` as `{"tool_id","name","output"}` meaning the call's output, and `grep 0041 docs/adr` finds nothing, while ADR-0040 left "**Amended by ADR-0040:** …" at ADR-0019:67. A reader of the contract alone also cannot learn that `output` changed meaning (full → preview) — semantic, not additive. *Fix:* one "**Amended by ADR-0041:**" note under ADR-0019:69 naming the three keys and the truncation of `output`.

**P3 · `src/rpc.rs:181-192`** — the diff deleted `Mapping`'s summary line, so its doc comment now opens with an empty `///` and the sentence "One value per turn, because …" starts it; `:189-192` is a ragged reflow ("…the provider's `toolcall_*` blocks and a tool's / partial results. Declining them is the / design…"). *Fix:* restore the deleted line, reflow the paragraph.

**P3 · `src/tool.rs:89`** — `Stored.truncated` is computed and never read by production code (`ToolEnd::frame`, `src/rpc.rs:151-178`, uses only `preview` and `reference`); only unit tests read it. *Fix:* drop it, or surface it in the frame as a documented key.

**P3 · `src/rpc.rs:593-599`** — `result_text` returns `result.to_string()` for a result with no `content`, so `"result": null` becomes the text `null` in the file and in the preview. *Fix:* `None | Some(Value::Null) => String::new()`.

**P3 · `src/tool.rs:221`** — `file.sync_all()` is an fsync on a tokio worker per tool call, unexplained, while `SessionLog::append` (`src/exit.rs:260-289`) does not sync. The sync-fs-in-async shape itself matches the repo (`backend/docker.rs:393`); only the fsync is unrecorded. *Fix:* one sentence of why, or drop it.

**P3 · `src/tool.rs:24-29, :152-166`** — "a traversal is impossible by construction" holds for the *id*, but `Store::read` uses `std::fs::read`, which follows a symlink named `<tool_id>.txt` planted in `<data>/sessions/<id>/tool/`. Not reachable from the container (its mounts are the worktree and `pi-home`), so this is a documentation gap: state that the threat model is "a name the agent chose", not "anything under `<data>`".

**P3 · `docs/build.md:117-226`** — the prose enumeration of `serve_cases` has no clause for `case_tool_output`, while every other case in the array has one (`…case_history`, which boots its own server with `history_window = 1` …; see ADR-0033) and `docs/build.md:228` states the "every later story adds at least one `case_*`" rule. *Fix:* one clause beside the session-screen sentence.

**P3 · `RESUME.md:46-47, :64, :67`** — the plan's file list names `RESUME.md`; the diff does not touch it. It still says `HEAD` is `a602c6f`, "Decisions are ADRs `0001` … `0040`", "Next issue: `PER-65`". *Fix:* the bookkeeping refresh, or drop the claim from the plan.

## Spec

**P1 · `docs/adr/0041-tool-output-is-a-file.md:46-51`, `src/rpc.rs:379-381, :415, :593-613`** — bullet 1's "exit status" is answered by `status: ok|error`, and the ADR justifies that with two statements the code contradicts:

1. "a `bash` tool's status code lives inside `result` (`structuredContent.exit_code` in the captured turn), … which **the file holds verbatim** … anything finer is one `output_ref` away" (ADR-0041:48-51). The file holds only `result.content`'s **text blocks**; `structuredContent`/`details` are dropped, so the exit code — which the committed fixture does carry (`tests/fixtures/pi-turn-tool.jsonl`, `"structuredContent":{…,"exit_code":0,…}`) — is not recoverable anywhere.
2. "the whole call is in the message that asked for it and in the stored result's own record" (ADR-0041:46-47, repeated at `src/rpc.rs:379-381`). `message.end`'s `tool_calls` carries **names only** (`src/rpc.rs:629-647`) and the file holds the result, not the arguments — so a `write` whose args exceed `ARGUMENTS_MAX` has its body stored nowhere.

*Fix:* either write the whole `result` (and `args`) into the file — the honest reading of "the full output … inspectable" and of the ADR's own promise — or correct ADR-0041:46-51 and `src/rpc.rs:379-381` to say plainly that neither the exit code nor truncated arguments are retained.

**P2 · `web/src/main.ts:554-557`** — the arguments summary never reaches the screen: `tool-start` sets only `change.name`, and `arguments` appears nowhere in `main.ts` (grep: no match) though `Frame`/`Change` carry it (`web/src/transcript.ts:28, :53`). The person is "**I want to see what the agent ran**"; the operator sees `bash` and its output, never the command. Phase 1b forbids *requiring* a UI change, but the frame and the block both exist. *Fix:* one line drawing the summary (ADR-0041:45 already caps it at 2000 chars).

**P3 · `docs/adr/0041-…` (the cap sections)** — the demo's "a tool that prints 2 MB" is not reachable against pi: its own `bash` tool truncates to "last 2000 lines or 50KB" (the tool description committed in `tests/fixtures/pi-turn-tool.jsonl`), so `CAP` is defensive and the 2 MB/6 MiB cases are synthetic (`tests/rpc.rs:519-560`) or planted (`scripts/verify.sh:1376`). One sentence would stop the 5 MiB cap being read as the ordinary case.

**Correct (spec).** Bullet 2: path, cap and marker are right, including the case the brief asks about — an output of exactly `CAP` bytes writes no marker and a file of exactly `CAP` bytes (`src/tool.rs:206-222`, `floor_boundary` `:228-238`). Bullet 3: `output` is the preview and `output_ref` appears only when the write succeeded (`src/rpc.rs:151-178`), and the pump stores before it appends, so a reference is never ahead of its file (`src/rpc.rs:495-517`). Bullet 4: the route is a `GET` (hence `ReadOnly` by `tests/guards.rs:769-800`), checks the row before any directory (`src/http/tool_output.rs:56-72`) and re-checks the id; `..`, `%2e%2e%2f…`, `a/b`, 129 chars, a non-ASCII id and the session id itself are refused before a join (`src/tool.rs:102-118, :198-200`, `src/exit.rs:314-322`). Bullet 1: exactly one `tool.start`/`tool.end` pair per call — `toolcall_*` and `tool_execution_update` are *declined*, not unknown, so a tool turn's `warn` count is 0 (`tests/rpc.rs:377-385`). Bullet 5, behaviourally: nothing on this path writes `session.log`; only the test's proof is weak (P1). Docs: `docs/stories/README.md:117` says "Forty-one" and `docs/adr/` holds 0001–0041 with the new bullet's link resolving; the fixture's record multiset that I counted by hand (22 non-`message_update` records + 47 = its 69 lines) matches the drift guard's `("message_update", 47)` and the rest.

**Old durable rows do not break (brief item 7).** `parseFrame` maps the three absent keys to `null` (`web/src/transcript.ts:193-203`); `web/src/main.ts:558-568` guards on `null`, so `meta` stays empty (`style.css:229-231` hides it) and the anchor stays hidden — `.tool-full { display: inline-block }` (`style.css:233-237`) does not defeat `hidden`, because the file's own `[hidden] { display: none }` (`style.css:311-317`) comes later at equal specificity. `data-tool`/`href` are set as attributes, never HTML.

### Verified by reading

- `src/rpc.rs:245-255` — the tools upper bound; the three declined and two mapped records resolve through
  `exit.rs:314`'s allow-list without a shell escape.
- The cap arithmetic at exactly `CAP` (no marker, file size exactly `CAP`) and above it.
- `tests/rpc.rs:1-...`: fixture multiset by hand — 69 lines = 22 fixed records + 47 `message_update`.
- `st..`: `pump` stores before it appends (`src/rpc.rs:495-517`).
- Manifest: a `GET` is `ReadOnly` by derivation, not by a table (`tests/guards.rs:769-800`).
- Contract docs: `docs/stories/README.md:117` (41 decisions) against `docs/adr/` (41 files) and the link resolving.
- Page: `[hidden]` beats `.tool-full`; old rows render exactly as before (`web/src/transcript.ts:193-203`, `web/src/main.ts:554-568`, `web/src/style.css:229-237, 311-317`).
- The fixture's `structuredContent.exit_code` exists and is dropped by `result_text` (`src/rpc.rs:593-613`).
- `src/exit.rs:314-322` is a character allow-list, so `tool_id` and `session_id` both fail closed.
- `scripts/verify.sh:2882-2892`'s `sed '/^\r$/q'` reasoning against `fetch`'s full-body read (`:161-181`) — the SIGPIPE fix is sound.

### Could not verify without running

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — including whether the drift guard's numbers pass exactly and whether the 6 MiB case stays inside its timing budget.
- `npm test` and `tsc --noEmit` (`web/package.json` scripts; `scripts/build-web.sh:51, :53`) for the web changes.
- `scripts/verify.sh` end to end: `case_tool_output`'s 2 MiB answer under the harness, the `case_no_cors_header` sweep over 2 MiB response files (whether the 141 is really gone), and the run's added cost.
- axum's decoding of `%FF`, `%00` and `%2F` segments (`PathRejection → ""`), and the guard's behaviour on a top-level anchor navigation to `/api/…`.
- Whether real pi ever emits `"result": null` or a result without `content`, and whether `sync_all` per tool call measurably delays the pump.

**Summary — Standards:** 10 findings, worst is the P1 test that cannot fail (`tests/rpc.rs:503-515`). **Spec:** 3 findings, worst is the P1 false claim about `output_ref`/the file holding the exit code and the whole call (`docs/adr/0041-…:46-51`). **Merge verdict: OK with notes** — mergeable as it stands; both P1s should land before the story is marked Done. A supervisor should run `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`, then `scripts/verify.sh`.

---

## Resolution

Every finding was checked against the code before it was acted on; "fixed" means the change is in this
story's fix commit, and the mutation or the command that shows it is named where it matters.

### Standards

| Finding | Verdict | What changed |
| --- | --- | --- |
| P1 · `tests/rpc.rs:503-515` — the `session.log` test cannot fail (the file is never created) | **Fixed** | The test now appends a stderr line through `exit::SessionLog` (a new `Log::sink()`), asserts the file exists and holds that line, and *then* asserts the tool's text is absent. |
| P2 · `tests/rpc.rs:496-501` — the pump test never proves a preview | **Fixed** | The pump test drives a synthetic 4096-byte tool turn: the log's `output` is `PREVIEW_MAX + 1` characters ending in the ellipsis, the file holds all 4096 bytes, and the file is asserted longer than the frame. |
| P2 · `tests/tool_output.rs:122-166`, `scripts/verify.sh:1362-1408` — both traversal checks pass with `valid_id` deleted | **Fixed, and the reviewer's diagnosis was sharpened by measurement** | Axum *does* decode a `%2F` inside a segment (measured with a print in the handler under a mutated `valid_id`), and the stored name is `<id>.txt`, so the reachable canary is one level up and the id must omit the extension: `..%2Fsecret` → `<data>/sessions/<id>/secret.txt` (Rust test), `..%2F..%2Fleak` → `<data>/sessions/leak.txt` (gate). **Mutation-proved**: with `valid_id` replaced by `true`, `an_id_that_cannot_be_a_filename_is_not_a_file_to_read` fails with the canary's own text; with it restored, the suite is green. |
| P2 · ADR-0019 is not amended in place | **Fixed** | ADR-0019 now carries an "**Amended by ADR-0041:**" note under its payload table, naming the three additive keys and — explicitly — that `tool.end`'s `output` changes *meaning* from the whole output to a preview. |
| P3 · `src/rpc.rs:181-192` — ragged `Mapping` doc comment | **Fixed** | The summary line is restored and the paragraph reflowed. |
| P3 · `src/tool.rs:89` — `Stored.truncated` is never read in production | **Fixed** | The field is gone; the file's own marker line is the one statement that an output was cut, and the store test asserts the marker. |
| P3 · `src/rpc.rs:593-599` — `"result": null` became the text `null` | **Fixed** | `result_text` answers the empty string for `Value::Null`, and its doc says why. |
| P3 · `src/tool.rs:221` — an unexplained `sync_all` per tool call | **Fixed** | Dropped. The file is written and the frame follows it; neither this file nor `session.log` claims a durability the log itself does not. |
| P3 · symlinks: "impossible by construction" overstates the id rule | **Fixed (documented)** | The module doc and ADR-0041 now name the threat model — a *name the agent chose* — and record that a symlink planted inside `<data>/sessions/<id>/tool/` would be followed, which nothing in this build can create. |
| P3 · `docs/build.md` prose enumeration lacks a clause for `case_tool_output` | **Fixed** | One clause beside the session-screen's, naming the 2 MiB answer, the byte-for-byte check, the traversal ids and why the pump's own storage is a Rust test's. |
| P3 · `RESUME.md` is stale (HEAD, ADR count, next issue) | **Fixed** | Refreshed in its own commit, after the ship note. |

### Spec

| Finding | Verdict | What changed |
| --- | --- | --- |
| P1 · ADR-0041's "exit status" rests on two false claims (the file holds `structuredContent`; the whole call is elsewhere) | **Fixed** | ADR-0041 and `rpc.rs` now say plainly what is **not** retained: the file holds the result's text blocks only, so a `bash` exit code in `details`/`structuredContent` is dropped, and the arguments beyond `ARGUMENTS_MAX` live nowhere. `status` is therefore the tool's verdict, which is this story's reading of the acceptance's "exit status" — and keeping a `details` blob or an exit code as a key is named as a new decision with its own story (HS-035 touches a result's non-text parts anyway). |
| P2 · the arguments summary never reaches the screen | **Fixed** | The tool block draws the summary (`tool-arguments`, filled by `tool.start`, `textContent`, hidden when empty) — the story's own words are "I want to see what the agent ran", and a block that named the tool and hid the command did not answer them. |
| P3 · pi's own `bash` already truncates, so the 5 MiB cap reads as the ordinary case | **Fixed (documented)** | ADR-0041 records that the cap is defensive: pi's `bash` promises the last 2000 lines or 50 KB (its own description, in the committed fixture), so the six-megabyte cases are synthetic and the gate's two megabytes planted; a project-local extension's tool need not truncate, which is the case the cap is for. |

### What the review's "could not verify without running" list found

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — green (660 tests).
- `npm test` (37) and `tsc --noEmit` — green.
- `scripts/verify.sh` end to end — green: **52 cases, 466 assertions**, both musl targets, the CORS sweep over
  the 2 MiB answer included. The 141 this story first hit is gone (see the previous commit); the added cost is
  one `dd` and one 2 MiB read.
- Axum's decoding of `%2F` — measured: it decodes, which is why the canary could be placed where a naive join
  lands, and why the reviewer's suggested path needed one more correction (the `.txt` extension).
- `"result": null` and `sync_all` — the first is now handled, the second removed.

### Acknowledgements

The reviewer's P1 on the `session.log` bullet and its P2 on the preview were both about *tests that could not
fail*, which is the third time this run has been caught by that shape; the traversal finding was right about
the canary and one step short of the reason (the appended extension), which the mutation showed.
