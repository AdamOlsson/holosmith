# History truncation and pagination

This is spec item 15's third sentence: "a new device opening a session must not hang the phone". The log
is append-only in spirit and unbounded in practice — a 200k-event session is what a long turn looks like —
and until this story a client opening the stream with no `Last-Event-ID` was replayed **all** of it. The
story decides where a replay starts, what it says about what it skipped, and how a client gets the rest.

## The window is a config key, and `0` is refused

`history_window`, `Kind::Integer`, default 50. The story says "the 50-message window is configurable", and
ADR-0006 says a machine setting is a row in `KeySpec` or it does not exist. `0` is refused rather than
meaning "no history": a client shown nothing cannot tell an empty session from a truncated one, and the
frame that would explain the difference is the one it would not be sent.

## The cut is the lowest seq of the window's oldest message

"The beginning of the 50th most recent message" is a **backwards** scan over the two kinds that name a
message and carry its text (`message.delta`, `message.snapshot` — compaction replaces a run of the first
with the second, so both are needed and `message.end` alone would miss a folded message's text). The scan
reads descending seq and collects distinct message ids until one more than the window has been seen; the
cut is the **lowest** seq belonging to a message inside the window.

Lowest, not last: a block must be whole. Cutting at a message's final row would replay half of it, and the
renderer's append rule has no way to say "this text is incomplete".

**That rule rests on one producer property, stated here rather than assumed:** a message's
`message.delta`/`message.snapshot` rows are contiguous in seq. ADR-0018 pins one block per message id and
every producer today writes a message's frames together, so the first row of a message older than the
window is also the last row of the window's oldest message plus one. A producer that interleaved two
messages would be cut mid-block, with no `message.end` inside the replay to repair it. Closing that would
mean scanning down to the window's lowest `message.end` — a second rule about what a message *is*, in the
one place that currently reads only message ids — and no story asks for it.

The scan stops the moment a row belongs to a message older than the window, so its cost is the newest
`window` messages' rows rather than the session's log — which is the whole point, since the log is the thing
that got too big to send. A row whose payload names no message id belongs to no block, so it neither
consumes a place in the window nor ends the scan; the compactor and the writer's coalescer read an
unexpected payload the same forgiving way.

`None` — nothing truncated — is decided by whether an older message was actually **found**, not by whether
the window filled. A log of exactly `window` messages has its cut at its own first row, so its replay
already starts at the beginning and a truncation frame would be a claim about nothing.

## The cut is computed in the read step, and only for a client with no resume point

A reconnecting client sends `Last-Event-ID` and already has everything below its own point; cutting under it
would throw away events it is entitled to. So the window applies only to `after == 0`, which
`http::streams::resume_point` produces for every absent, empty or unparsable header.

The cut and the replay are read in **one** synchronous scope inside the stream's existing subscribe → read →
forward order (ADR-0014), so the two cannot disagree about where the log starts, and the database mutex is
still never held across an `await`.

## `history.truncated` carries one key more than the spec names

The spec's payload is `{before_seq}`. The frame also carries `turns_before`, and it is load-bearing: the
page numbers a turn from the `turn.start` frames it has **seen**, so a replay starting at the 50th message
would label its first turn "turn 1" while the operator's log says turn 40. `turns_before` is a count of
`turn.start` rows below the cut, and it is the one number the client cannot compute — the frames that would
have said it are exactly the ones it was not sent.

The frame has **no SSE `id`**, for `stream_lagged`'s reason (ADR-0014): it is not a stored event, so a
browser that resumed after it would resume past a seq the log does not have. The extra key is additive and
this frame is not in ADR-0018's pinned table — it is the stream's own kind, like `stream_lagged`, and lives
next to it in `events/stream.rs` rather than in the stored kinds' block.

## A page is `seq < before ORDER BY seq DESC LIMIT n`, and that is stable

`GET /api/sessions/{id}/events/history?before=<seq>&limit=200` answers
`{events, before_seq, has_more, turns_before}`. The cursor is exclusive and `before_seq` is the page's
**lowest** seq, so one number serves both "where the replay started" and "the next request" — a client never
computes a seq of its own.

Stability under concurrent writes is free rather than engineered: seq is strictly increasing, so an event
committed after the read has a seq above every row a page can hold. The only thing that moves a page is
compaction removing rows (ADR-0032), which is the log's documented gap rule and not a cursor problem.

The page is read newest-first and reversed, so a full page is the rows nearest the cursor; `has_more` comes
from reading `limit + 1` rows and is not a second statement. `before` is **required** and a new
`422 bad_query` refuses one that is not a positive integer — "the newest page" is what the stream already
gave the client, so a missing cursor has no safe default. `limit` is clamped to 1000 rather than refused,
because "give me everything" is a paging mistake and not a malformed request.

## The affordance is a button, and a page is folded in a fragment

"Load earlier" above the transcript, hidden until the truncation frame arrives, one page per click, hidden
again when `has_more` is false. Not infinite scroll: the story says so, and one click is one request.

A page is folded through a **fresh** `Transcript` whose turn base is the page's `turns_before`, into a
detached `DocumentFragment`, which is then `prepend`ed in one insertion. The page is strictly older than the
cursor the stream was opened at, so its blocks are new ids and no live block is touched; the fragment is what
keeps the page's cost one DOM write rather than one per node.

`transcript.ts` therefore gains exactly three rules and no DOM: `parseTruncated` (a frame with no usable
cursor is dropped, and a missing turn count is 0 — wrong about labels, not about content), `setTurnBase`
(the one spelling of the base, used by the live model and by each page's), and `historyPath`.

## What the gate shows

`case_history` boots its own server with `history_window = 1` — the smallest window that truncates — drives
two turns, and asserts the truncation frame's cursor and turn count, that the replay's first `id:` is that
cursor, that the page below it carries the first turn with `has_more` false and `turns_before` 0, and that a
`before=0` is `422 bad_query` with the envelope's content type. `tests/history.rs` covers the same rules from
a hand-written log, plus the two a gate case cannot reach: a log that fits its window is not truncated, and
a page does not shift when events arrive above it.

What no test here can show is a phone: the story's premise is a client that would have hung, and the
measurement that matters — a 200k-event session opening quickly — is the operator's, on their device.

The **"Load earlier" affordance itself** is proved by reading rather than by a test, and that is the
repository's own division of labour: no script may require a browser (`docs/build.md`), so the button's
reveal, its one-page-per-click fold and its hiding when `has_more` is false live in `web/src/main.ts`
where nothing without a DOM can reach them. Everything about them that *is* a rule — the frame's payload,
the turn base, the page size, the path — is in `web/src/transcript.ts` and is tested with node's own
runner.
