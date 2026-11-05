# The transcript's event contract, one block per message id, and a renderer without a browser

HS-014 is the first screen that renders agent output, and it lands one story before the first
producer of that output (HS-015). That order is deliberate — the story slicing puts the renderer
first because its acceptance bullets are all renderer-side — but it leaves two things that a later
story would otherwise decide by accident: the **wire shape** of a delta, and the **cost rule** that
keeps a 10,000-event turn from melting a phone. Both are cheap now and expensive once HS-015, HS-032
and HS-033 have each made their own choice.

## The four event kinds, pinned before their producer exists

A frame on `GET /api/sessions/{id}/events` is an SSE event whose name is the kind and whose data is
one JSON object:

| Kind | Payload | Meaning |
| --- | --- | --- |
| `message.delta` | `{"message_id": "…", "kind": "text" \| "thinking", "text": "…"}` | one incremental piece of a message |
| `message.snapshot` | `{"message_id": "…", "text": "…", "thinking": "…"}` | the whole message, replacing its deltas |
| `turn.start` | `{}` | a turn began |
| `turn.end` | `{}` | a turn ended |

**Amended by ADR-0043:** there is a fifth kind, `turn.error`, carrying `{"reason", "method"}` and **no message
id** — one frame, one block, nothing continues it, which is the shape ADR-0021 gave a steer. Its one producer is
an extension's dialog, which a session cannot answer and therefore cancels; the sentence it carries is why an
extension's behaviour is explicable rather than mysterious.

`session.status` keeps its ADR-0016 payload and drives the screen's badge; these four drive the
transcript. Two of the four names are the spec's, quoted from item 15: `message.snapshot` "holding
full text, thinking, message id", and the coalescing rule "consecutive deltas of the same kind and
message". That rule is why `kind` is a payload field rather than a second event name: one message
holds both kinds of output, so a consumer needs to know which kind a delta belongs to, and
coalescing is per `(message_id, kind)` without splitting the wire into `message.text.delta` and
`message.thinking.delta`. `message.delta` itself is this story's name for the piece item 15 calls a
delta, and `turn.start` / `turn.end` are this story's: item 15 never spells a turn event, and the
only source for them is HS-015's acceptance bullet ("Turn start and end are explicit events"), so
they are pinned here to keep HS-015 from choosing a second spelling.

Three rules follow, and they are what HS-015's producer must satisfy:

- **A message id is the key.** Deltas and snapshots for one id land in one block. An id is the
  producer's to mint; nothing here spells its format, because only uniqueness per session matters.
- **An absent kind is the empty string, not "leave it".** A snapshot is the message's whole state, so
  a text-only snapshot clears thinking that deltas had already put on screen, and a snapshot naming
  neither is an empty message rather than a frame to drop. This is what "replacing rather than
  appending" means once a block has two kinds. Only `message_id` is mandatory.
- **A snapshot is the last frame for its id.** Nothing here can enforce that — the page appends a
  delta that follows one — and nothing the server can produce today violates it (item 15's snapshots
  come from compacting messages completed more than 24 hours ago). HS-015 and HS-060 are the stories
  that could, and this sentence is what they have to keep.
- **A boundary carries no payload this screen reads.** `turn.end` will want a stop reason; that is
  HS-015's decision, and reading no payload here means the decision costs this story nothing. A
  frame whose kind this screen does not know — `tool.start`, `usage`, later kinds — is dropped, so
  HS-032 through HS-034 add kinds without a page release. **Superseded in part by ADR-0019:** HS-015
  emits `message.end`, `tool.start` and `tool.end`, and the page renders them; the drop rule now
  covers the kinds no story has named yet.

## One block per message id, and a delta is an append

The block is `article[data-message]` holding one node per kind, `.thinking` and `.text`. A delta
appends **one text node** to its kind's node:

```ts
node.appendChild(document.createTextNode(piece))
```

and nothing already on screen is read back. The obvious alternative — `node.textContent += piece` —
reads the accumulated text and writes it back, so one turn's rendering cost grows with the square of
its own length; at 10,000 deltas that is the difference between a screen that keeps up and one that
stalls. A snapshot is the only replacing operation: `replaceChildren` on both nodes, with an
emptied one hidden again so a cleared thinking band leaves no gap.

Block lookup is a `Map<message_id, nodes>` filled as frames arrive, never a document query, so no
frame costs a walk over the transcript. The transcript container is a bounded, scrollable pane
(`max-height: 60vh`) so a long turn cannot push the composer off a phone's screen; the DOM itself is
not truncated or paged — HS-060's compaction and HS-061's history paging own that, and a third
truncation rule in the page would be a second source of truth about what history is.

## The screen is a path, not a router

`/sessions/<id>` is not a route on the server: the fallback already answers the app shell for every
non-`/api/` path (ADR-0007), so a reload of one session's screen lands in `main.ts` with nothing
installed. The page reads its id from `location.pathname` (`sessionIdFromPath`) and fetches
`GET /api/sessions/{id}`, which is this story's one new endpoint: the bare `Session` row, or
ADR-0008's `not_found`. It answers the row and not a bundle with the transcript, because the
transcript is a stream and a history fetch is HS-061's.

The list and the screen share `streamPath(id)` and no resume parameter: the browser resends the last
SSE `id` it saw as `Last-Event-ID` and the server replays after it (ADR-0014), so a reconnect
resumes exactly after what is on screen and the page needs no cursor.

The composer exists in every state and its three actions come from `affordances(state)`: prompt on
`idle` (the `prompt` trigger's row), steer and abort on `running`, nothing on `starting`, `stopped`,
`error` or `deleting`. The endpoints are later stories' — **prompt is HS-015's, abort HS-016's,
steer HS-017's** — so pressing one names the story that will wire it rather than sending a request
that would 404. **Superseded by ADR-0019:** the prompt is wired; abort and steer still name their
story. **Superseded by ADR-0020:** abort is wired too; only steer still names its story.
**Superseded by ADR-0021:** steer is wired too, so no composer action names a later story; a steer's
block is the one block a frame creates without a message id. **Superseded by ADR-0022:** a fourth
action, Resume, exists in `stopped` and `error`, so "nothing" is true only of `starting` and `deleting`. The form also swallows its own submit: with no `action` a native submit is a GET of
the current path, which would reload the screen and discard the transcript the operator is reading. That is the same choice
HS-013 made for the retention placeholder: a control that cannot work yet says which story makes it
work.

## The renderer's rules are tested without a browser

This repository's gates run on a machine with no browser, and HS-014 is one story before the first
event producer. Both facts point at the same split: the rules live in `web/src/transcript.ts` —
`parseFrame`, the block model, `affordances`, `sessionIdFromPath` — and are folded over synthetic
frames by `web/test/transcript.test.ts`, using **node's own test runner** (`node --test`). Node 24
strips TypeScript types natively, so this adds no dependency and leaves `package-lock.json` alone
(ADR-0017's rule). `web/package.json` gains one `test` script and `scripts/build-web.sh` runs it
before the build, which is what puts the transcript's rules inside `scripts/verify.sh`'s build
phase instead of in a step nobody runs.

`web/test/` sits outside `tsconfig`'s `include`, so `tsc --noEmit` does not typecheck the test file;
typing it would mean adding `@types/node`, and the runner is the authority that executes it either
way. The extension in its `../src/transcript.ts` import is what node's ESM resolver requires.

What this leaves uncovered is stated rather than hidden: the DOM half of the screen — node creation,
`hidden` toggling, the composer's handler wiring — is **unverified from source** until a browser or
HS-015's producer exists. The next story's demo is the first that can look at it.

The screen keeps its stream open in every state, unlike a list row (ADR-0017 closes a row's source as
soon as it leaves a live state). The two rules are not in conflict: a row is a summary of something
else, while the screen *is* the session the operator asked for by name, and a `stopped` session's
history is exactly what they opened it to read. HS-019 ends the stream when a session is deleted.

## Consequences

- `GET /api/sessions/{id}` answers the bare row, so it carries `client_key` exactly as the list does:
  one field set, one decoder. An unknown id — and a `%FF` segment that is not UTF-8, which the route
  maps into the same answer instead of axum's `text/plain` 400 — is ADR-0008's `not_found`.
- `crates/holosmith/tests/session_screen.rs` proves the row and its fifteen columns, the `not_found`
  envelope with its code and content type, the guard refusal on a foreign `Host`, the `/sessions/<id>`
  fallback serving the shell byte-for-byte, and the route's `ReadOnly` manifest entry.
- `scripts/verify.sh` grew `case_session_detail` (the row against the created id, the absent `events`
  key, the unknown id, the malformed segment) and `case_session_screen` (the shell bytes), from 21
  cases and 66 assertions to 23 and 82.
- `web/test/transcript.test.ts` is twelve tests over synthetic frames, run by `npm test` inside
  `scripts/build-web.sh`, so the transcript's rules fail the build phase rather than a step nobody
  runs. The DOM half of the screen is the part that stays unverified until HS-015's demo.
