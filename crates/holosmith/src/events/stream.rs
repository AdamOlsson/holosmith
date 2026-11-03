//! The SSE body, and the subscribe-replay-live assembly behind it (HS-010, ADR-0014).
//!
//! `Sse` wants a `futures_core::Stream`, which the workspace has no combinator for, so the one
//! impl this crate needs is written out here over the channel a task fills. The task is where
//! resume correctness lives, and its order is the story:
//!
//! 1. **subscribe** to the writer's live broadcast,
//! 2. **read** the session's committed history,
//! 3. hand over the history, then this session's live events, skipping any seq the replay
//!    already sent.
//!
//! Subscribe after the read and an event committed in between belongs to neither half: it is not
//! in the history the read saw and it was broadcast before the subscription existed. Nothing
//! later can notice it, so the client loses it for good. Subscribing first can only produce a
//! duplicate, and the `seq <= last_sent` check is what removes it.
//!
//! The writer has **one** broadcast channel for every session (ADR-0013), so step 3 also drops
//! an event whose `session_id` is not this stream's — before the seq comparison, because a
//! foreign seq means nothing here and would otherwise move `last_sent` past this session's own
//! next seq, silently dropping events that do belong to it.
//!
//! A failed send is how the task usually learns the client went away, but a session with nothing
//! to send never fails a send: the loop's reclaim tick is what releases a registration left by a
//! client that vanished while the session was silent.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::response::sse::Event;
use futures_core::Stream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use super::Event as StoredEvent;
use super::registry::{StreamGuard, StreamRegistry};
use super::{EventStream, EventWriter, RepoError};
use crate::db::Database;

/// The event kind a client listens for to learn it fell behind.
pub(crate) const STREAM_LAGGED: &str = "stream_lagged";

/// How many frames the body may hold before the task waits for the client to read them.
///
/// This is the lag detector as much as a buffer: a client that stops reading fills it, then the
/// subscription behind it, and the subscriber is told it lagged instead of the server growing.
/// The writer's own buffer (`SUBSCRIBER_BUFFER`) is what decides how much can be skipped.
const BODY_QUEUE: usize = 64;

/// How much history one replay reads: all of it.
///
/// Deliberately not a window. HS-061 owns "the 50th most recent message" and the paged
/// `GET /api/sessions/:id/events/history`; a truncation rule invented here would be that story's
/// decision, taken in the wrong one.
const REPLAY_LIMIT: u32 = u32::MAX;

/// How often an idle stream checks whether its client is still there.
///
/// The body's channel is the only thing that notices a dropped response body, and a send is only
/// attempted when there is a frame to send. So a client that vanishes while its session is silent
/// would otherwise park the task forever with its registration alive — and HS-066 reads that count
/// to decide whether to push. A few seconds: one timer wake per open stream, and the count is only
/// ever read for a decision, never polled.
const RECLAIM_INTERVAL: Duration = Duration::from_secs(5);

/// What the task hands the body. Framing happens once, in [`Item::frame`], so the replay/live
/// rules below can be tested on plain events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// One committed event, in seq order.
    Event(StoredEvent),
    /// This subscriber fell behind by `skipped` seqs, so the stream ends after this frame.
    Lagged { skipped: u64 },
}

impl Item {
    /// The SSE frame: `id` is the seq (D1), so the client's `Last-Event-ID` is exactly the seq
    /// the replay resumes after. A lag report carries no `id`, so a browser keeps the last real
    /// seq and reconnects from there.
    fn frame(&self) -> Event {
        match self {
            Self::Event(event) => Event::default()
                .id(event.seq.to_string())
                .event(event.kind.clone())
                .data(event.payload.clone()),
            Self::Lagged { skipped } => Event::default()
                .event(STREAM_LAGGED)
                .data(serde_json::json!({ "skipped": skipped }).to_string()),
        }
    }
}

/// The response body: frames the task hands over, in order, until it ends.
pub(crate) struct EventBody {
    items: mpsc::Receiver<Item>,
}

impl Stream for EventBody {
    type Item = Result<Event, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().items.poll_recv(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(Ok(item.frame()))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Start a session's stream after `after` and return the body axum serves.
///
/// The three steps are in the module comment and the order is the point: subscribe, read, then
/// forward. `Err` is a history read that failed before anything was streamed, so the caller can
/// still answer an envelope instead of a half-open connection.
pub(crate) fn attach(
    writer: &EventWriter,
    database: &Mutex<Database>,
    registry: &StreamRegistry,
    session_id: &str,
    after: i64,
) -> Result<EventBody, RepoError> {
    attach_with(
        writer,
        registry,
        session_id,
        after,
        RECLAIM_INTERVAL,
        |session_id, after| {
            // One synchronous scope, released before the task starts: the lock is never held
            // across an await (ADR-0013).
            let database = database.lock().unwrap_or_else(PoisonError::into_inner);
            database
                .events()
                .read_since(session_id, after, REPLAY_LIMIT)
        },
    )
}

/// [`attach`] with the read step and the reclaim interval handed in.
///
/// Both exist so a test can decide what the read sees and when an idle stream looks for its
/// client: subscribe-before-read is only a rule if a test can tell the two orders apart, and a
/// read that broadcasts before it returns loses its event under the wrong one.
fn attach_with(
    writer: &EventWriter,
    registry: &StreamRegistry,
    session_id: &str,
    after: i64,
    reclaim: Duration,
    read: impl FnOnce(&str, i64) -> Result<Vec<StoredEvent>, RepoError>,
) -> Result<EventBody, RepoError> {
    let subscription = writer.subscribe();
    let history = read(session_id, after)?;
    let session = session_id.to_string();
    let guard = registry.open(session_id);
    let (items, body) = mpsc::channel(BODY_QUEUE);
    tokio::spawn(async move {
        // The guard lives in the task, so every way the task can end — the client gone, the
        // session closed, a lag report, the writer stopped — drops it and releases the count.
        let mut guard = guard;
        forward(
            subscription,
            &session,
            after,
            reclaim,
            &history,
            &items,
            &mut guard,
        )
        .await;
    });
    Ok(EventBody { items: body })
}

/// Send the replay, then every live event of this session that it did not already carry, then
/// end.
///
/// `RecvError::Lagged` ends the stream after one `stream_lagged` frame: the client reconnects
/// with `Last-Event-ID` and this same replay fills the gap from the database, so the
/// subscriber is dropped without anything being lost (D5). Carrying on silently would leave a
/// client wrong about what it has, which is worse than making it reconnect.
async fn forward(
    mut subscription: EventStream,
    session_id: &str,
    after: i64,
    reclaim: Duration,
    history: &[StoredEvent],
    items: &mpsc::Sender<Item>,
    guard: &mut StreamGuard,
) {
    // The resume point, not zero: with `Last-Event-ID: n` and an empty replay the skip rule below
    // would be vacuous, and a live event with `seq <= n` would be forwarded as if it were new.
    let mut last_sent = after;
    for event in history {
        if hand(items, guard, Item::Event(event.clone()))
            .await
            .is_none()
        {
            return;
        }
        last_sent = event.seq;
    }
    let mut reclaim = tokio::time::interval(reclaim);
    loop {
        let received = tokio::select! {
            () = guard.closed() => return,
            // The client may have gone while the session is silent, when no send will fail: the
            // response body is dropped, its receiver with it, and this branch is the only thing
            // that notices. It writes nothing — the heartbeat stays axum's `KeepAlive` alone.
            _ = reclaim.tick() => {
                if items.is_closed() {
                    return;
                }
                continue;
            }
            received = subscription.recv() => received,
        };
        let event = match received {
            Ok(event) => event,
            Err(RecvError::Lagged(skipped)) => {
                let _ = hand(items, guard, Item::Lagged { skipped }).await;
                return;
            }
            // The writer stopped, which in a served process means the process is going down.
            Err(RecvError::Closed) => return,
        };
        // The writer's channel carries every session, so a foreign event is dropped before the
        // seq check: forwarding it would put another session's frames in this response, and its
        // seq would push `last_sent` past this session's own next seq.
        if event.session_id != session_id {
            continue;
        }
        // The subscription can carry an event the replay already sent: it was committed before
        // the read but broadcast after the subscribe. Sending it twice would make the client
        // apply it twice; skipping it is the whole reason for this line.
        if event.seq <= last_sent {
            continue;
        }
        if hand(items, guard, Item::Event(event.clone()))
            .await
            .is_none()
        {
            return;
        }
        last_sent = event.seq;
    }
}

/// Hand one frame to the body. `None` means this stream is over: the client is gone, or the
/// session was closed while the frame waited for room behind a client that is not reading.
async fn hand(items: &mpsc::Sender<Item>, guard: &mut StreamGuard, item: Item) -> Option<()> {
    tokio::select! {
        () = guard.closed() => None,
        sent = items.send(item) => sent.ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    const NOW: i64 = 1_760_000_000;

    const SESSION: &str = "0123456789abcdef";

    /// A reclaim interval the whole of a test's own work fits inside, for the tests about the
    /// forward loop rather than about the idle check.
    const NEVER_RECLAIM: Duration = Duration::from_secs(3600);

    /// The `EventStream` the handler would get from `EventWriter::subscribe`, made from a
    /// channel this module can drive directly: `stream` is inside `events`, so the field is in
    /// scope, and the test controls exactly when an event is broadcast relative to the replay.
    /// That interleaving is what the socket tests can only reach by timing.
    fn subscription(live: &broadcast::Sender<StoredEvent>) -> EventStream {
        EventStream {
            receiver: live.subscribe(),
        }
    }

    fn event(seq: i64) -> StoredEvent {
        event_of(SESSION, seq)
    }

    fn event_of(session_id: &str, seq: i64) -> StoredEvent {
        StoredEvent {
            session_id: session_id.to_string(),
            seq,
            kind: "session.status".to_string(),
            payload: format!("{{\"seq\":{seq}}}"),
            created_at: NOW,
        }
    }

    /// The writer handle the handler would hold, built over channels this module drives.
    fn writer(live: &broadcast::Sender<StoredEvent>) -> EventWriter {
        let (sender, _queued) = mpsc::channel(1);
        EventWriter {
            sender,
            events: live.clone(),
        }
    }

    /// Everything the body received, in order, until the task ended. A bounded wait, so a stream
    /// that never ends fails the test instead of hanging it.
    async fn drain(items: &mut mpsc::Receiver<Item>) -> Vec<Item> {
        let mut received = Vec::new();
        while let Some(item) = tokio::time::timeout(std::time::Duration::from_secs(5), items.recv())
            .await
            .expect("the stream ends")
        {
            received.push(item);
        }
        received
    }

    fn seqs(items: &[Item]) -> Vec<i64> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Event(event) => Some(event.seq),
                Item::Lagged { .. } => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn an_event_in_both_the_replay_and_the_subscription_arrives_once() {
        // The window the acceptance calls out: the event was committed before the history read
        // (so the replay carries it) *and* broadcast after the subscribe (so the subscription
        // carries it). It is in both halves, and the client must see it once.
        let (live, _) = broadcast::channel(8);
        let subscription = subscription(&live);
        let replay = [event(1), event(2)];
        live.send(event(2)).expect("a subscriber exists");
        let mut guard = StreamRegistry::new().open("0123456789abcdef");
        let (items, mut body) = mpsc::channel(BODY_QUEUE);

        let forwarding = tokio::spawn(async move {
            forward(
                subscription,
                SESSION,
                0,
                NEVER_RECLAIM,
                &replay,
                &items,
                &mut guard,
            )
            .await;
        });
        // The writer stops, so the live half ends once its queued event is read.
        drop(live);
        let received = drain(&mut body).await;
        forwarding.await.expect("the task does not panic");

        assert_eq!(
            received,
            [Item::Event(event(1)), Item::Event(event(2))],
            "the event committed inside the replay window was duplicated or dropped"
        );
    }

    #[tokio::test]
    async fn an_event_committed_after_the_replay_is_not_lost() {
        // The other side of the same window: committed after the read, broadcast after the
        // subscribe. Only the live half has it, so the subscription is the only reason it is
        // delivered at all.
        let (live, _) = broadcast::channel(8);
        let subscription = subscription(&live);
        let replay = [event(1)];
        live.send(event(2)).expect("a subscriber exists");
        let mut guard = StreamRegistry::new().open("0123456789abcdef");
        let (items, mut body) = mpsc::channel(BODY_QUEUE);

        let forwarding = tokio::spawn(async move {
            forward(
                subscription,
                SESSION,
                0,
                NEVER_RECLAIM,
                &replay,
                &items,
                &mut guard,
            )
            .await;
        });
        drop(live);
        let received = drain(&mut body).await;
        forwarding.await.expect("the task does not panic");

        assert_eq!(
            seqs(&received),
            [1, 2],
            "the event committed after the replay was lost"
        );
    }

    #[tokio::test]
    async fn a_lag_report_is_one_frame_and_ends_the_stream() {
        // A buffer of four with eight events already committed: the subscriber can only be told
        // it missed the first four, and the stream then ends so the client reconnects.
        let (live, _) = broadcast::channel(4);
        let subscription = subscription(&live);
        for seq in 1..=8 {
            live.send(event(seq)).expect("a subscriber exists");
        }
        let mut guard = StreamRegistry::new().open("0123456789abcdef");
        let (items, mut body) = mpsc::channel(BODY_QUEUE);
        let forwarding = tokio::spawn(async move {
            forward(
                subscription,
                SESSION,
                0,
                NEVER_RECLAIM,
                &[],
                &items,
                &mut guard,
            )
            .await;
        });
        let received = drain(&mut body).await;
        forwarding.await.expect("the task does not panic");

        assert_eq!(
            received,
            [Item::Lagged { skipped: 4 }],
            "a lagging subscriber was not told exactly what it missed"
        );
    }

    #[tokio::test]
    async fn closing_the_session_ends_the_stream() {
        let (live, _) = broadcast::channel(8);
        let subscription = subscription(&live);
        let registry = StreamRegistry::new();
        let mut guard = registry.open("0123456789abcdef");
        let (items, mut body) = mpsc::channel(BODY_QUEUE);
        let forwarding = tokio::spawn(async move {
            forward(
                subscription,
                SESSION,
                0,
                NEVER_RECLAIM,
                &[],
                &items,
                &mut guard,
            )
            .await;
        });

        registry.close("0123456789abcdef");
        let received = drain(&mut body).await;
        forwarding.await.expect("the task does not panic");
        assert!(received.is_empty(), "a closed stream sent {received:?}");
        assert_eq!(registry.count("0123456789abcdef"), 0, "the count leaked");
    }

    #[tokio::test]
    async fn another_sessions_event_is_not_forwarded() {
        // The writer has one broadcast channel for every session, so a foreign event is dropped
        // — and dropped before the seq check, so its larger seq cannot move `last_sent` past
        // this session's own next seq and lose a real one.
        let (live, _) = broadcast::channel(8);
        let subscription = subscription(&live);
        live.send(event(1)).expect("a subscriber exists");
        for seq in 1..=3 {
            live.send(event_of("fedcba9876543210", seq))
                .expect("a subscriber exists");
        }
        live.send(event(2)).expect("a subscriber exists");
        let mut guard = StreamRegistry::new().open(SESSION);
        let (items, mut body) = mpsc::channel(BODY_QUEUE);
        let forwarding = tokio::spawn(async move {
            forward(
                subscription,
                SESSION,
                0,
                NEVER_RECLAIM,
                &[],
                &items,
                &mut guard,
            )
            .await;
        });
        drop(live);
        let received = drain(&mut body).await;
        forwarding.await.expect("the task does not panic");

        assert_eq!(
            seqs(&received),
            [1, 2],
            "a foreign event leaked into this session's stream"
        );
    }

    #[tokio::test]
    async fn a_live_event_at_or_before_the_resume_point_is_not_forwarded() {
        // `Last-Event-ID: 2` with an empty replay: the client already has seqs 1 and 2, so a
        // live seq 2 must not be forwarded again. From `last_sent = 0` it would be.
        let (live, _) = broadcast::channel(8);
        let subscription = subscription(&live);
        live.send(event(2)).expect("a subscriber exists");
        live.send(event(3)).expect("a subscriber exists");
        let mut guard = StreamRegistry::new().open(SESSION);
        let (items, mut body) = mpsc::channel(BODY_QUEUE);
        let forwarding = tokio::spawn(async move {
            forward(
                subscription,
                SESSION,
                2,
                NEVER_RECLAIM,
                &[],
                &items,
                &mut guard,
            )
            .await;
        });
        drop(live);
        let received = drain(&mut body).await;
        forwarding.await.expect("the task does not panic");

        assert_eq!(
            seqs(&received),
            [3],
            "a seq the client had already resumed past was forwarded again"
        );
    }

    #[tokio::test]
    async fn attach_subscribes_before_it_reads() {
        // The read step broadcasts seq 2 and then answers with seq 1, so seq 2 is delivered only
        // if the subscription existed before the read ran. Read first and it is broadcast to
        // nobody: the stream would lose it for good, which is the hole this order closes.
        let (live, _) = broadcast::channel(8);
        let writer = writer(&live);
        let registry = StreamRegistry::new();

        let mut body = attach_with(
            &writer,
            &registry,
            SESSION,
            0,
            NEVER_RECLAIM,
            |_session_id, _after| {
                live.send(event(2)).expect("the subscriber exists");
                Ok(vec![event(1)])
            },
        )
        .expect("the stream opens");

        // The writer stops, so the live half ends once its queued event is read.
        drop(writer);
        drop(live);
        let received = drain(&mut body.items).await;

        assert_eq!(
            seqs(&received),
            [1, 2],
            "the read ran before the subscribe: the event committed inside it was lost"
        );
    }

    #[tokio::test]
    async fn an_idle_stream_releases_its_count_and_writes_nothing() {
        // Nothing is appended and nothing is closed, so no send can fail: the reclaim tick is the
        // only thing that can notice the response body was dropped.
        let (live, _) = broadcast::channel(8);
        let writer = writer(&live);
        let registry = StreamRegistry::new();
        let mut body = attach_with(
            &writer,
            &registry,
            SESSION,
            0,
            Duration::from_millis(10),
            |_session_id, _after| Ok(Vec::new()),
        )
        .expect("the stream opens");
        assert_eq!(registry.count(SESSION), 1, "the stream did not register");

        // Several ticks pass with the body still there: the tick writes no frame, because the
        // heartbeat is axum's `KeepAlive` and nothing else's.
        let wrote = tokio::time::timeout(Duration::from_millis(100), body.items.recv()).await;
        assert!(wrote.is_err(), "an idle stream wrote a frame of its own");

        // Dropping the body is what a client that went away does to it.
        drop(body);
        let released = tokio::time::timeout(Duration::from_secs(5), async {
            while registry.count(SESSION) > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(
            released.is_ok(),
            "an idle stream kept its count after the client left"
        );
    }
}
