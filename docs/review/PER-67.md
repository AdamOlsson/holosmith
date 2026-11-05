## Review — PER-67 / HS-035 (`b2fecf5..HEAD`)

Scope note: I reviewed the delta from `/tmp/holosmith/reviews/PER-67-diff.txt` plus the current tree (clean), the two plans, ADR-0043, ADR-0018/0019/0041, `docs/build.md`, `RESUME.md`, `GLOSSARY.md`, and the story slice. I could not list the four commits themselves (no git), so everything below is about the diff/hunks as supplied.

---

## Standards — verdict: **OK with notes**

### Correct
- **The mapping stays pure; the I/O is the pump's.** `Mapped::Cancel`/`ToolEnd.images` are finished in `pump` (`crates/holosmith/src/rpc.rs:657-670`, `:644-655`), and `Mapped::into_frame` answers a store-less `tool.end` with **no** `images` key (`rpc.rs:199-206`), so a committed fixture still replays with no data root — precisely ADR-0040/0041's rule.
- **The cancel is written before the frame**, with a `warn` (not a failure) if the write is refused (`rpc.rs:657-668`), matching ADR-0043 word for word.
- **Route hygiene is identical to `tool-output`.** `.get(...)` on `Routes` ⇒ manifest `ReadOnly` (`http/app.rs:507-517`); the fallible extractor is mapped by the handler (`unwrap_or_default`), the row is read before any directory (`http/tool_image.rs:52-64`), the id is allow-listed before any join (`:65-67`), and the index is parsed so `..`/`/` cannot be a name (`:69-72`). `tests/guards.rs:786-805` iterates the manifest, so the four guards cover the new route with no list to update.
- **No new dependency** (hand-written `decode_base64`), **no new config key** (`IMAGE_CAP`/`PREVIEW_MAX` stay constants), no SQL touched, no `awk`/`shellcheck`, no pi-version literal added.
- **Gate rule honoured:** `case_tool_image` is added to `serve_cases` (`scripts/verify.sh:316`) and the case is registered before the CORS sweep; it asserts status, `content-type`, and byte equality with `cmp` (`verify.sh:1428-1495`).
- **Doc conventions:** ADR-0018 and ADR-0019 got "Amended by ADR-0043" notes rather than rewrites; `docs/stories/README.md` got both the 43 count and the bullet **contiguously after ADR-0042** (`docs/stories/README.md:161`), which is the rule at `RESUME.md:1128` and the failure mode PER-93 caught; `docs/build.md`'s edited bullets are reflowed to ~100 columns (`docs/build.md:345-383`); two glossary entries added with `_Avoid_` lines.

### Findings
- **S1 · P3 — ADR-0041 has no "Amended by ADR-0043" note, although it pins the payload that changed.** `docs/adr/0041-tool-output-is-a-file.md:35-42` spells `tool.end`'s payload as its final word, and `:63-64` is where it states the convention ("ADR-0019 now carries an 'Amended by ADR-0041' note"). ADR-0019 got the note for the same `images` key (`docs/adr/0019-…:80-85`); ADR-0041 did not. *Smallest fix:* one paragraph after the payload block, mirroring ADR-0019's.
- **S2 · P3 — `RESUME.md` was not refreshed, though both plans name it** (`/tmp/holosmith/plans/PER-67.md` §Docs, `PER-67-linear.md` §Docs "plus `RESUME.md`'s refresh"). It is now false in at least six places: `:68` "Next issue: `PER-67`"; `:65` and `:776` "ADRs `0001` … `0042`"; `:314` "`rpc::pump(records, mapping, events)`" (five arguments now); `:366-374` the kind table (no `turn.error`, `tool.end` without `images`); `:803-804` "**Not built yet:** … HS-035"; `:587` "**53 cases plus a CORS sweep (490 assertions)**" (54 cases now). Precedent (PER-65:37, PER-66:S8) grades exactly this P3 and fixes it in the handoff commit — so: a note for that commit, not a defect of this one.
- **S3 · P3 — `Agent::respond` duplicates `Agent::request`'s timed stdin write** (`crates/holosmith/src/agent.rs:485-491` vs `:532-538`): same lock, same `write_all`/`flush`, same three-way error mapping. *Possible Duplicated Code.* *Smallest fix:* a private `write_line(&self, bytes: &[u8], bound: Duration) -> Result<(), AgentError>` both call.
- **S4 · P3 — two copies of the same base64 encoder in test code:** `crates/holosmith/src/tool.rs:712-731` (`encode_base64`) and `crates/holosmith/tests/tool_output.rs:245-262` (`base64`), identical bar the name. The unit copy must stay inside the lib, but `tests/tool_output.rs` could take one from `tests/support/mod.rs` (already the shared harness).
- **S5 · P3 — a second error red in one stylesheet.** `web/src/style.css:154-156` already has `.error { color: #b3261e }`; `.turn-error` introduces `#c0392b` (`:285`), and it is the only raw colour in the file otherwise (`currentColor` everywhere, `.steer` at `:266`). *Fix:* reuse `#b3261e`.
- **S6 · P3 — ADR-0043's memory claim is about the wrong stage.** `docs/adr/0043-…:37-38` says "a record claiming a gigabyte of image must not be a gigabyte of memory on the way to being refused", but `agent::read_records` reads an unbounded line (`crates/holosmith/src/agent.rs:594-606`, `read_until`) and `result_images` clones the whole base64 into `tool::Image` (`rpc.rs:787-810`) *before* `decode_base64`'s gate runs (`tool.rs:463`). The decode-side bound is real; the sentence should say it bounds the **decode**, not the record.
- **S7 · P3 — the cap is 5 MiB + 2 bytes.** `if end / 4 * 3 > IMAGE_CAP` (`tool.rs:463`) ignores the trailing group: an unpadded body with `end % 4 == 3` decodes to `end/4*3 + 2`, so `IMAGE_CAP + 2` bytes are stored, against `IMAGE_CAP`'s own doc ("The most bytes one stored image holds", `tool.rs:52-59`) and `docs/build.md:381-383` ("The cap is 5 MiB **decoded** per image"). Unpadded bodies are explicitly supported (`tool.rs:497-516` tests `aGVsbA`). *Fix:* `let decoded = end / 4 * 3 + match end % 4 { 2 => 1, 3 => 2, _ => 0 };`.

---

## Spec — verdict: **OK with notes**

### Correct
- **Every acceptance bullet has a mechanical check.** Bullet 1: the pump stores the picture beside the text and the frame names it, and no byte of it is in the payload (`tests/rpc.rs:1120-1180`, including `!payload.contains("iVBORw0KGgo")`); the text keeps `[image]` (`rpc.rs:753-773`; asserted `"Read image file [image/png][image]"` at `tests/rpc.rs:1030`). Bullet 2: the mapping returns `Mapped::Cancel` for `select|confirm|input|editor`, declines the fire-and-forget five, and counts an unknown method or a dialog with no id as the `warn` (`rpc.rs:521-545`), with the sentence built in one function (`:188-192`). Bullet 3: both new fixtures are in the replay tests and in the record-type multiset guard (`tests/rpc.rs:327-368`). Bullet 4: `cargo test` is a gate phase (`scripts/verify.sh:3166`), so the three V8 cases replay in the gate.
- **The cancel is proved to have *reached* the extension**, not merely been attempted: a fake agent that records its own stdin (`tests/rpc.rs:473-497`) and an assertion that exactly one line was written, with the request's own id (`tests/rpc.rs:1228-1265`); plus the fixture's own `notify` ("the dialog returned undefined (cancelled)"), asserted at `tests/rpc.rs:1097-1108`.
- **`turn.error`'s shape is exactly as decided:** two keys, no message id, asserted as `payload.as_object().len() == 2` (`tests/rpc.rs:1078-1085`), and the client's parse/apply path drops a reasonless frame (`web/src/transcript.ts:195-203`, `:303-307`; two new tests).
- **The committed fixtures are internally consistent with the guard:** 43 and 17 lines, summing exactly to the multiset totals asserted for each.
- **The image/dialog split matches spec V8's "If it fails" column** (auto-cancel and emit an error event) and the story's non-goal framing; nothing in the diff answers a dialog, and `RESPOND_TIMEOUT` is the pump's, not a caller's argument.

### Findings
- **P1 — acceptance bullet 1's "rendered **or linked**" is met on the wire, not on the screen; ADR-0043 says otherwise.** `tool.end` names `images[].output_ref` (`rpc.rs:245-260`) and the route serves it (`http/tool_image.rs`), but nothing in the page consumes the key: the tool-end `Frame`/`Change` carry no `images` (`web/src/transcript.ts:26-41`, `:56-68`), `parseFrame` never reads it (`:217-229`), and `applyChange` draws only `output` plus the `outputRef` anchor (`web/src/main.ts:562-591`). The operator's screen shows "Read image file [image/png][image]" and no way to see the picture. Meanwhile `docs/adr/0043-…:101` states "The frame names a URL and a mime, **and the page links it**" — two sentences after declaring rendering "a screen decision … no story owns it yet". The story slice's demo is "a tool result containing an image renders (or links) correctly" (`docs/stories/phase-1b-docker-backend.md:193`). *Smallest fix, pick one:* (a) parse `images` in `transcript.ts` and render one `<a>` per image in the tool block (the `.full` anchor at `main.ts:588-591` is the existing pattern), or (b) if the intended reading is "the frame is the link", correct ADR-0043 and record "the page does not draw it yet" as an explicit non-goal with an owner. I flag it as P1 because the choice is currently made implicitly and the ADR asserts the wrong one.
- **P2 — "A second result for one tool id replaces the first" (`docs/adr/0043-…:40-45`, `crates/holosmith/src/tool.rs:236-244`) is only true per index.** `store_images` calls `remove_siblings` for the indices the *new* result carries (`tool.rs:265-272`, `:406-425`), so an image the earlier result carried at an index the later one lacks stays on disk and still answers its route although no frame names it — a stale file, not a dead reference. Unlikely in practice (a `toolCallId` is one call), so: either weaken the ADR sentence to "replaces the file at the same index; indices the later result does not carry are HS-043's keep-rules' problem", or sweep the directory prefix.
- **P3 (report-only) — two file-layout nits worth knowing about, not fixing now:** `tests/tool_output.rs` now hosts two routes (`:1-4` says so), which is defensible but leaves the one-seam-per-file pattern; and `Cancel::frame()` returns `Mapped` (`rpc.rs:176-182`), so `into_frame` matches on the value it just built (`:209`) rather than on a `(kind, payload)` pair — a `Middle Man` of one line, harmless.

---

## Verified by reading

| Claim | Evidence |
|---|---|
| Mapping is I/O-free; store-less `tool.end` has no `images` | `rpc.rs:199-206`, `:226-260`; `tests/rpc.rs:1035-1045` |
| `tool.end` gains `images` only when stored | `rpc.rs:245-256`; `tests/rpc.rs:1130-1140` |
| Cancel written before the frame, `warn` on failure | `rpc.rs:657-670` |
| Response line repeats the request's id, `cancelled: true` | `rpc.rs:171-174`; `tests/rpc.rs:1206-1212`, `:1245-1250` |
| Exactly one stdin line written for a dialog + a notify | `tests/rpc.rs:473-497`, `:1230-1243` |
| `turn.error` payload = `{reason, method}`, no message id; one producer | `rpc.rs:176-192`; `tests/rpc.rs:1078-1085`; `events/mod.rs:57-63` |
| The sentence names the method and the outcome | `rpc.rs:188-192`; `tests/rpc.rs:1068-1078` |
| Text keeps `[image]`; base64 never enters a payload | `rpc.rs:753-773`; `tests/rpc.rs:1030`, `:1155-1160` |
| Route: row first, allow-list, numeric index, one `404 not_found`, type from the file | `http/tool_image.rs:44-83`; `http/app.rs:507-517` |
| Guards/`ReadOnly` cover the new route with no new list | `http/app.rs:221-240`; `tests/guards.rs:786-805` |
| Route mirrors `tool-output` exactly (same tuple form, same envelope) | `http/tool_output.rs:39-72` vs `http/tool_image.rs:44-83` |
| Every `pump` call site updated to five arguments | `backend/docker.rs:673`; `tests/rpc.rs:598,720,777,835,903,949,1133,1228` |
| Text and image files cannot collide; no traversal by construction | `tool.rs:184-200`, `:380-386`, `:406-425`, `:528`; `tests/tool_output.rs:300-345` |
| Second result replaces the same index's type | `tool.rs:265-272`, `:406-425`; `tests/tool.rs` (`a_second_result_replaces…`) |
| `IMAGE_CAP`+decoder bounds are checked before allocation | `tool.rs:463`; unit test at `tool.rs` (`base64_is_decoded…`) |
| Fixture guards' totals equal each fixture's line count (43, 17) | `tests/rpc.rs:330-368` vs the two committed `.jsonl` |
| Client parses/applies `turn.error`; drops a reasonless one | `transcript.ts:47,68,86,195-203,303-307`; `web/test/transcript.test.ts:465-522` |
| Page renders the block with `createElement`/`textContent` only | `main.ts:552-560` |
| The page consumes **no** `images` key | `transcript.ts:26-41,217-229`; `main.ts:588-591` (only `outputRef`) |
| `docs/stories/README.md` count + contiguous bullet | `docs/stories/README.md:117,161`; rule at `RESUME.md:1128` |
| ADR-0018/0019 amended, not rewritten | `docs/adr/0018-…:22-26`, `0019-…:80-85` |
| Gate case added and registered | `scripts/verify.sh:316`, `:1428-1495` |
| `docs/build.md` prose reflowed and accurate | `docs/build.md:345-383` |
| No new dependency / config key / SQL; no pi-version literal added | `Cargo.toml` untouched by the diff; `tool.rs:59` a `const`; `tests/pin_single_source.rs` not touched |

## Could not verify without running
- `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check` — including that `tests/rpc.rs`'s `skipped == 8` for the extension fixture holds (I hand-counted the arms and it is consistent, but it is not proof), and that the unit test's `Scratch`/`encode_base64` helpers compile as written.
- `npm test` / `npx tsc --noEmit` in `web/` (the `Change`/`Frame` unions are extended; the diff adds one ~140-column line in `web/test/transcript.test.ts:503`, which is outside any configured lint I could find).
- `scripts/verify.sh` end to end, and therefore the *shipped-artifact* half of `case_tool_image` (`expect_header` compares the first matching header; whether axum emits one `content-type` for `(Status, [(CONTENT_TYPE, …)], Vec<u8>)` is inherited from `tool_output`'s identical shape, so I judge the risk low but unproven).
- That the two fixtures are verbatim captures from `pi-agent:local` (the tree proves internal consistency and the guard, not provenance), and that pi's dialog methods behave as `docs/rpc-extension-ui.md` is cited to say — that document is pi's, not in this repo.
- That the recording fake agent's `cat > "$2"` writes each line promptly enough for the 10 ms poll loop (timing, not logic).

**Summary:** Standards — 7 findings, worst S1 (P3, ADR-0041's missing amendment note); Spec — 3 findings, worst the P1 that the image is served but never linked by the page while ADR-0043 claims it is. No P0; nothing here argues against merge once the P1 is decided one way or the other.
---

## Resolution

Every finding is triaged below. The two that mattered were the Spec P1 (the picture was served and never
linked, while ADR-0043 said the page linked it) and the P2 (a second result replaced only the indices it
carried); both were fixed in code, with the test that makes the wrong design fail.

| Finding | Verdict | What changed |
| --- | --- | --- |
| Spec **P1** — the image is served but nothing in the page consumes `images`, while ADR-0043 says "the page links it" | **Fixed** (option a) | `transcript.ts` gains a `ToolImage` type, parses `images` (an entry that cannot be linked is dropped, an absent key is `[]`, so an old row renders as before) and carries it on the `tool-end` change; `main.ts` draws one `<a>` per picture (`image 1 · image/png`) in the tool block, `createElement`/`textContent` only; `style.css` gives it a row. ADR-0043's non-goal is reworded: the link is the story's "rendered **or** linked", drawing the picture is what nothing owns. New web test. |
| Spec **P2** — "a second result replaces the first" was true only per index | **Fixed** | `tool::store_images` decodes first, then sweeps every `<tool_id>-<digits>.<known extension>` the new set does not name (`tool::sweep`), so an index the later result dropped is gone and a changed type leaves no stale file; the digit requirement is what keeps a tool whose id merely begins with this one untouched. New unit test with a two-image result followed by a one-image result *and* a neighbour tool id, which is the case a prefix match gets wrong. ADR-0043's sentence now says "the whole set". |
| **S1** P3 — ADR-0041 has no amendment note, though it pins the payload that changed | **Fixed** | "Amended by ADR-0043" added after its payload block, naming `images` and correcting its "an image block is not retained". |
| **S2** P3 — `RESUME.md` is stale (next issue, ADR count, `pump`'s arguments, the kind table, the gate's case count) | **Fixed** | Refreshed in the handoff commit, which is this repository's precedent for it. |
| **S3** P3 — `Agent::respond` duplicated `Agent::request`'s timed stdin write | **Fixed** | One private `Agent::write_line(line, bound)`; `request` gains only the `forget` on failure and `respond` is one line. |
| **S4** P3 — two identical base64 encoders in test code | **Fixed** | One `support::base64`, used by `tests/tool_output.rs`; the lib's `encode_base64` stays in `tool.rs`'s `cfg(test)` module, because a test binary cannot see it and the decoder is what it is paired with. |
| **S5** P3 — a second error red (`#c0392b`) in a stylesheet that already had `#b3261e` | **Fixed** | `.turn-error` reuses `#b3261e`. |
| **S6** P3 — ADR-0043's memory claim was about the wrong stage | **Fixed** | The sentence now bounds the **decode**, and records that `agent::read_records` reads one unbounded line, so a huge body is already in memory when this check runs — a reader decision with its own story rather than something this cap covers. |
| **S7** P3 — the cap allowed `IMAGE_CAP + 2` on an unpadded body | **Fixed** | The trailing group is counted (`+1` for two characters, `+2` for three) and a unit test asserts `IMAGE_CAP` decodes, `+1` and `+2` are refused. |
| Report-only P3 — `tests/tool_output.rs` hosts two routes now; `Cancel::frame` returns `Mapped`, so `into_frame` matches on what it just built | **Dismissed**, with the reason | The two routes are one directory's two halves (a call's text and its pictures) and the file's own doc says so; splitting them would put a shared `NO_*`/row-check preamble in two files. `Cancel::frame` returning `Mapped` is deliberate: `ToolEnd::frame` does the same, so "make the one frame this variant answers" has one shape rather than two. |
