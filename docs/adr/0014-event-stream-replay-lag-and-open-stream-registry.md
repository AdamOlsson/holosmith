# The session event stream: subscribe before replay, drop a lagging subscriber, count what is open

HS-008's create answers with an `events` URL and HS-009 builds the durable log behind it; HS-010 is
the endpoint that joins them. Four decisions come due at once. The stream has to resume from a
`Last-Event-ID` without a hole between the history it replays and the events it forwards. A
subscriber that stops reading must not be allowed to grow the server or hold up another subscriber.
"Stream closes cleanly when the session is deleted" needs something that knows which sessions have a
stream open — HS-019 will close them and HS-066 will ask before it pushes. And the 15 s heartbeat has
to exist without becoming a second thing that writes to the same connection.

The spec settles none of the four beyond the acceptance bullets, so this records the shape chosen
and the evidence it rests on.

## Measured before deciding

- **axum 0.8.9 ships SSE.** `Sse::new` takes `S: TryStream<Ok = Event> + Send + 'static`
  (`axum-0.8.9/src/response/sse.rs:62-66`, which is the blanket `Stream<Item = Result<…>>` bound),
  and its `IntoResponse` writes `content-type: text/event-stream` and `cache-control: no-cache`
  (`sse.rs:86-100`), with `Event::id`/`event`/`data` writing the three fields this stream needs.
  `KeepAlive::new()` already defaults to `Duration::from_secs(15)` (`sse.rs:524-527`) and its event
  is `Event::DEFAULT_KEEP_ALIVE`, the literal `:\n\n` (`sse.rs:201`), which hyper sends as its own
  body frame.
- **`futures_core::Stream` is not nameable without the crate as a direct dependency.** `Sse` wants
  it, and implementing it means writing the trait's path. `axum::body` re-exports `HttpBody` and
  `Bytes` (`axum-0.8.9/src/body/mod.rs:4,7`) but not `http_body::Frame`, which a hand-written
  `HttpBody` would need (`sse.rs:38` imports it privately); no other direct dependency re-exports
  the trait; `tokio` 1.54.1 has no `stream` feature. So the choice is a direct dependency or no
  stream at all.
- **`tokio::sync::mpsc::Receiver::poll_recv` exists**, so the body's `Stream` impl is a `match` over
  it and needs no combinator.
- **An SSE response never EOFs.** `scripts/verify.sh`'s `fetch` reads with `timeout 10 cat` and
  treats a non-zero exit as a failed request, so it would report a working stream as a failure after
  ten seconds. A second helper is required, not a tuning of the first.
- **A broadcast receiver answers its buffered values and then `Closed`** once the sender is dropped,
  which is what lets the reassembly tests end a stream without a shutdown flag.

## `futures-core` becomes a direct dependency

`Sse::new` cannot be given a body without naming `futures_core::Stream`, and the trait is only in
the tree transitively, so `futures-core = "0.3"` is declared in `[workspace.dependencies]` like every
other dependency and used as `futures-core.workspace = true`. It adds **no package and no download**:
`Cargo.lock` already carries `futures-core` 0.3.34 for axum, and the only lock change is the
`holosmith -> futures-core` edge. The alternative — `tokio-stream`, for `ReceiverStream` — would add a
package and a `StreamExt` surface this crate does not otherwise use, and was rejected in the plan.

## Subscribe, then replay, then forward

The handler's task does three things in this order, and the order is the story:

1. `EventWriter::subscribe()` — a broadcast receiver from this instant on;
2. `EventRepository::read_since(session, after, …)` — the committed history;
3. send the history, then forward live events, **skipping any whose `seq <= last_sent`**.

Read first and subscribe second, and an event committed between the two is in neither half: it was
not in the rows the read saw, and it was broadcast before a receiver existed. Nothing later can
notice it, so the client is missing an event for good. Subscribe first can only produce a
*duplicate*, and one comparison removes it. The window is small but it is not zero, and it is
exactly the case the acceptance calls out.

`read_since` takes `u32::MAX` as its limit rather than a page size: this story replays everything
after the client's last seq, and HS-061 owns "the 50th most recent message", `history.truncated` and
`GET …/events/history`. A window invented here would be that story's decision made in the wrong one.

The writer has **one** broadcast channel for every session (ADR-0013), so step 3 drops an event whose
`session_id` is not this stream's, and drops it **before** the seq comparison. Forwarding a foreign
event would write another session's frames — carrying no session field, so the client could not even
notice — into this response. Comparing seqs first would be worse: `last_sent` is one scalar, so a
foreign event with a larger seq would move it past this session's own next seq and the client would
silently lose every event up to that number. `last_sent` starts at the resume point rather than at
zero for the same reason: with `Last-Event-ID: n` and an empty replay the skip rule would otherwise be
vacuous and a live event with `seq <= n` would be forwarded as new.

Proved at both levels. `events::stream`'s unit tests build the `EventStream` from a broadcast channel
they drive and hand the task a snapshot, so they can produce each interleaving exactly:
`an_event_in_both_the_replay_and_the_subscription_arrives_once` (committed before the read, broadcast
after the subscribe — the duplicate the filter exists for),
`an_event_committed_after_the_replay_is_not_lost` (broadcast after the subscribe only),
`another_sessions_event_is_not_forwarded` (a foreign event between this session's own two),
`a_live_event_at_or_before_the_resume_point_is_not_forwarded`, and
`attach_subscribes_before_it_reads`, which is the one that fails if the two steps are swapped: it hands
`attach_with` a read step that broadcasts an event and *then* returns a snapshot, so the event is
delivered only if the subscription already existed. From a socket the window can only be reached by
timing, so `tests/streams.rs` gives the writer a 500 ms commit window and appends inside it:
`an_event_committed_during_the_replay_window_is_neither_lost_nor_duplicated` asserts the client sees
`id: 1` then `id: 2`, once each. `a_stream_carries_only_its_own_sessions_events` opens a stream on one
session, appends one event for it, three for another session with larger seqs, then one more for the
first, and asserts it sees exactly `[1, 2]` and nothing in the meantime.

## A lagging subscriber is dropped, not silently continued

Each subscriber has its own task and a bounded channel of 64 frames in front of the response body. A
client that stops reading fills the channel, the task blocks on the send, and the subscription behind
it falls through the writer's 1024-event ring. The writer never waits for any of this: `broadcast::send`
is what HS-009 measured as non-blocking, so one slow client cannot delay a commit or another session's
stream.

On `RecvError::Lagged(skipped)` the task sends one frame — `event: stream_lagged`, `data:
{"skipped":n}`, and deliberately **no `id`**, so the browser keeps its last real seq — and then
returns, which ends the response and the connection. The client reconnects with
`Last-Event-ID: <last real seq>` and the replay fills the gap from the database.

Carrying on silently was the alternative and it is worse in the way that matters: a client that is
told nothing is a client that shows a transcript with a hole in it and no way to know. "Dropped with
a `stream_lagged` event" plus a reconnect loses nothing and tells the client the truth.

`tests/streams.rs` proves it with payloads large enough that the socket buffers cannot hide the
volume: the slow connection ends with exactly that frame — asserted to carry no `id` — and then EOF,
while the writer commits every event. `a_reconnect_after_a_lag_refills_the_gap` then resumes from
inside the gap and asserts the replay picks up at the seq after it, which is the recovery the missing
`id` exists to make possible. `a_slow_subscriber_does_not_delay_another` is the other half — the
subscriber that reads gets every event in order while the other one is not polled at all.

## The registry: a count per session, one close signal per stream

`StreamRegistry` is `Clone` (one `Arc`), lives in `AppState`, and holds per session an open-stream
count and one `oneshot::Sender<()>` per open stream. `open` returns a `StreamGuard`; `close(session)`
fires every signal for that session and answers how many it fired; `count` is what HS-066 reads later.
A "which sessions have an open stream" list has no caller yet, so it is not written: HS-066 adds it
back when it actually reads it.

The count is released by `StreamGuard`'s `Drop`, and the guard lives in the stream's task, so every
way a stream can end — `close` (the select wins), a lag report, the writer stopping, a panic unwinding
the task — decrements it exactly once. A client that goes away is noticed two ways, because a failed
send is only available when there is something to send: the send fails, or — for a session that stays
silent — the loop's reclaim tick finds the response body's receiver gone (`mpsc::Sender::is_closed`,
polled every few seconds). That branch writes no frame: the heartbeat is axum's `KeepAlive` and
nothing else's. Without it an idle stream whose client vanished would park forever and keep its count,
`closers` slot and registry entry, which is exactly the stale count HS-066 must not read. A line at the
end of the handler would not: the handler's future can be dropped mid-body when the client goes away,
and ADR-0007's drain deadline cuts open connections without running anything after them. A leaked
count would make HS-066 think a deleted session still has a browser attached.

Each registration carries an id, so a guard can remove its own closer on the way out; without it the
closer list would grow with every stream that ever opened on a long-lived session.

`close` is synchronous and idempotent, which is what HS-019's delete flow needs: firing the signals
is all this side can do, and the guards release the count as each stream ends.

## The keep-alive is axum's, and `Last-Event-ID` is the seq

`Sse::new(body).keep_alive(KeepAlive::new().interval(KEEP_ALIVE_INTERVAL))`, where
`KEEP_ALIVE_INTERVAL` is 15 s and public so a test can assert the number instead of waiting it out.
The `:\n\n` line is written by the same wrapper that knows the stream is idle, so nothing in this
crate calls `Event::comment`: a hand-written heartbeat would be a second writer on the connection and
would double the interval the story asks for. `the_keep_alive_is_fifteen_seconds` also asserts that
the body writes *nothing* for 300 ms, which is what a stray comment line would break.

The SSE `id` is the seq, so the client's `Last-Event-ID` needs no translation. `Last-Event-ID: n`
replays from `n + 1`; absent, empty, non-numeric and non-positive all start at the beginning, because
this story promises a whole stream and HS-061 owns the truncation rule.

The route is `GET /api/sessions/{id}/events`, registered through `Routes::get` so the manifest says
`ReadOnly` (axum 0.8 spells a capture `{id}`; the spec writes `:id`). Because it is a `GET`, the
`Host` guard applies to it with nothing added. An unknown session is ADR-0008's 404 envelope, decided
before the registry is touched, so an unknown id never registers a stream.

## What this story does not decide

No `history.truncated` or the 50th-message rule (HS-061), no `GET …/events/history` (HS-061), no
coalescing (HS-059), no compaction (HS-060), no Web Push (HS-066 reads `count` and nothing else
here), no delete flow (HS-019 calls `close`), no producer (HS-011), no UI, no new crate beyond the
`futures-core` edge above, and no change to the guards, to `fetch`, or to the sixteen `verify.sh`
cases that shipped. The `retry:` field, a per-session subscriber cap and a configurable channel depth
are all left unset rather than guessed.

## Consequences

- Every subscriber costs one task and one 64-frame channel; a session with nobody attached costs
  nothing, and a client that stops reading is disconnected after roughly 1025 undelivered events
  instead of being held forever.
- `scripts/verify.sh` grows one case and one helper: `case_events_stream` reads the header block of
  the real artifact's stream and asserts `200`, `text/event-stream` and `no-cache`. `fetch_stream`
  exists because `fetch` waits for EOF that an SSE response never sends, and the helper reads line by
  line under bash's own timeout so the header block is what ends the read.
- A future story that wants to *push* an event to one subscriber bypasses all of this by appending
  it: the writer is still the only path into the log, and the stream is only a reader.
