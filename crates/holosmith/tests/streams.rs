//! `GET /api/sessions/:id/events` over a real socket (ADR-0014).
//!
//! Each row of the story's acceptance is one test here, with two exceptions that are proved
//! where the code is rather than by waiting: the keep-alive interval
//! (`KEEP_ALIVE_INTERVAL` is asserted against the spec's 15 s, because a test that waited it
//! out would take fifteen seconds and prove less) and the exact interleavings of the replay
//! window (`events::stream`'s unit tests
//! drive the subscription and the snapshot by hand; from a socket the window can only be
//! reached by timing, which this file does for the end-to-end rule).
//!
//! The client speaks HTTP/1.0 on purpose: the body is then close-delimited instead of chunked,
//! so the harness reads the SSE bytes as they are written. Framing is the one thing it does not
//! parse, and the format a browser sees — `text/event-stream`, `id`/`event`/`data` blocks — is
//! unchanged by the version.

use std::time::{Duration, Instant};

use holosmith::clock;
use holosmith::config::Config;
use holosmith::events::{NewEvent, WriterOptions};
use holosmith::http::{self, AppState};
use holosmith::sessions::{NewSession, Session};

mod support;
use support::{Frame, PATIENCE, Served, Sse, TempDir};

/// The drain deadline each test gives `serve`, so a socket a test forgot to drop cannot hold the
/// server open for the production ten seconds.
const GRACE: Duration = Duration::from_millis(100);

fn create_session(state: &AppState, client_key: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .create(
            &NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: Some(client_key.to_string()),
                base_commit: None,
            },
            clock::unix_now(),
        )
        .expect("a session is created")
}

/// Queue one event for the writer. The payload names the kind, so a test can tell two events
/// with the same kind apart.
async fn append(state: &AppState, session: &Session, kind: &str) {
    append_payload(state, session, kind, &format!("{{\"kind\":\"{kind}\"}}")).await;
}

async fn append_payload(state: &AppState, session: &Session, kind: &str, payload: &str) {
    state
        .events
        .append(NewEvent {
            session_id: session.id.clone(),
            kind: kind.to_string(),
            payload: payload.to_string(),
            created_at: clock::unix_now(),
        })
        .await
        .expect("the writer accepts the event");
}

/// The newest seq a reader can see, so a test knows the writer committed before it connects.
fn committed(state: &AppState, session_id: &str) -> i64 {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .newest_visible_seq(session_id)
        .expect("the newest visible seq answers")
        .unwrap_or(0)
}

/// Poll until `condition` holds: a state the server reaches on its own.
async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "{what} did not happen within {PATIENCE:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn the_stream_is_sse_and_the_id_is_the_seq() {
    let dir = TempDir::new("streams-sse");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "sse");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut stream = Sse::connect(served.addr, &session.id, None).await;
    assert_eq!(stream.status, 200);
    assert_eq!(
        stream.header("content-type"),
        Some("text/event-stream"),
        "the route did not answer an event stream"
    );

    append(&state, &session, "session.status").await;
    let frame = stream.next_block(PATIENCE).await.expect("an event arrives");
    assert_eq!(frame.id.as_deref(), Some("1"), "the SSE id is not the seq");
    assert_eq!(frame.kind.as_deref(), Some("session.status"));
    assert_eq!(frame.data, "{\"kind\":\"session.status\"}");

    drop(stream);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn without_last_event_id_the_stream_starts_at_one() {
    let dir = TempDir::new("streams-from-beginning");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "from-beginning");
    for kind in ["first", "second", "third"] {
        append(&state, &session, kind).await;
    }
    wait_until("the three events commit", || {
        committed(&state, &session.id) == 3
    })
    .await;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut stream = Sse::connect(served.addr, &session.id, None).await;
    let frames = stream.take(3, PATIENCE).await;
    assert_eq!(
        frames.iter().map(Frame::seq).collect::<Vec<i64>>(),
        [1, 2, 3],
        "a client without a Last-Event-ID did not get the whole stream"
    );
    assert_eq!(frames[0].kind.as_deref(), Some("first"));

    drop(stream);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn last_event_id_resumes_at_the_next_seq() {
    let dir = TempDir::new("streams-resume");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "resume");
    for kind in ["first", "second", "third"] {
        append(&state, &session, kind).await;
    }
    wait_until("the three events commit", || {
        committed(&state, &session.id) == 3
    })
    .await;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut stream = Sse::connect(served.addr, &session.id, Some(1)).await;
    let frames = stream.take(2, PATIENCE).await;
    assert_eq!(
        frames.iter().map(Frame::seq).collect::<Vec<i64>>(),
        [2, 3],
        "Last-Event-ID: 1 did not resume at seq 2"
    );
    assert!(
        stream.quiet_for(Duration::from_millis(200)).await,
        "the replay repeated an event the client already had"
    );

    drop(stream);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn two_subscribers_see_the_same_order() {
    let dir = TempDir::new("streams-two");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "two");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut first = Sse::connect(served.addr, &session.id, None).await;
    let mut second = Sse::connect(served.addr, &session.id, None).await;
    let kinds = ["a", "b", "c", "d", "e"];
    for kind in kinds {
        append(&state, &session, kind).await;
    }

    let one = first.take(kinds.len(), PATIENCE).await;
    let two = second.take(kinds.len(), PATIENCE).await;
    assert_eq!(one, two, "two subscribers saw different frames");
    assert_eq!(
        one.iter().map(Frame::seq).collect::<Vec<i64>>(),
        (1..=kinds.len() as i64).collect::<Vec<i64>>()
    );
    assert_eq!(
        one.iter()
            .map(|frame| frame.kind.as_deref().unwrap_or_default())
            .collect::<Vec<&str>>(),
        kinds
    );

    drop(first);
    drop(second);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn a_stream_carries_only_its_own_sessions_events() {
    let dir = TempDir::new("streams-one-session");
    let state = support::state_in(&dir, Config::default());
    let a = create_session(&state, "own-events-a");
    let b = create_session(&state, "own-events-b");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut stream = Sse::connect(served.addr, &a.id, None).await;
    append(&state, &a, "a-one").await;
    let mut frames = stream.take(1, PATIENCE).await;

    // The writer has one broadcast channel for every session, so all three of B's reach A's
    // subscription: none of them may be written into A's response.
    for kind in ["b-one", "b-two", "b-three"] {
        append(&state, &b, kind).await;
    }
    wait_until("B's three events commit", || committed(&state, &b.id) == 3).await;
    assert!(
        stream.quiet_for(Duration::from_millis(300)).await,
        "another session's event was written into this stream"
    );

    // A's own second event has to survive B's larger seqs: a single last-sent seq moved by a
    // foreign event would drop it here.
    append(&state, &a, "a-two").await;
    frames.extend(stream.take(1, PATIENCE).await);
    assert_eq!(
        frames.iter().map(Frame::seq).collect::<Vec<i64>>(),
        [1, 2],
        "the stream carried another session's events, or lost this session's"
    );
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.kind.as_deref().unwrap_or_default())
            .collect::<Vec<&str>>(),
        ["a-one", "a-two"]
    );
    assert!(
        stream.quiet_for(Duration::from_millis(300)).await,
        "the stream carried an event that is not this session's"
    );

    drop(stream);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn a_slow_subscriber_does_not_delay_another() {
    let dir = TempDir::new("streams-slow");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "slow");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // The slow subscriber never reads a byte.
    let slow = Sse::connect(served.addr, &session.id, None).await;
    let mut fast = Sse::connect(served.addr, &session.id, None).await;
    let total = 100;
    for index in 0..total {
        append(&state, &session, &format!("event-{index}")).await;
    }

    let frames = fast.take(total, PATIENCE).await;
    assert_eq!(
        frames.iter().map(Frame::seq).collect::<Vec<i64>>(),
        (1..=total as i64).collect::<Vec<i64>>(),
        "the subscriber that reads did not get every event in order"
    );

    drop(slow);
    drop(fast);
    served.finish().await.expect("the app stops");
}

/// Drive `session`'s log past the writer's subscriber buffer while `slow` never reads, and answer
/// the frame `slow`'s stream ended with plus how many events the log holds.
///
/// The payload is large on purpose: the lag has to come from the server running out of room, so
/// the socket buffers must not be able to hold the volume.
async fn flood_until_lagged(state: &AppState, session: &Session, slow: &mut Sse) -> (Frame, i64) {
    let payload = format!("{{\"text\":\"{}\"}}", "x".repeat(8 * 1024));
    let total = 3_000_i64;

    // Produced while nothing reads, which is the case the acceptance names: a subscriber that
    // stops reading must not hold up the writer or another stream.
    let producer = {
        let state = state.clone();
        let session = session.clone();
        tokio::spawn(async move {
            for _ in 0..total {
                append_payload(&state, &session, "message.delta", &payload).await;
            }
        })
    };
    producer.await.expect("the producer task does not panic");
    let session_id = session.id.clone();
    wait_until(
        "every event commits with a subscriber that is not reading",
        || committed(state, &session_id) == total,
    )
    .await;
    // The commit and the broadcast are asynchronous to each other, so let the writer finish its
    // sends before reading the outcome (the same reason `tests/events.rs` sleeps here).
    tokio::time::sleep(Duration::from_millis(250)).await;

    // The stream that fell behind is dropped rather than left silently wrong: one
    // `stream_lagged` frame, then the end of the stream.
    let tail = slow.drain(PATIENCE).await;
    let last = tail
        .into_iter()
        .last()
        .expect("the slow stream sent something");
    (last, total)
}

#[tokio::test]
async fn a_lagged_subscriber_gets_stream_lagged_and_is_closed() {
    let dir = TempDir::new("streams-lagged");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "lagged");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut slow = Sse::connect(served.addr, &session.id, None).await;
    let (last, _total) = flood_until_lagged(&state, &session, &mut slow).await;
    assert_eq!(last.kind.as_deref(), Some("stream_lagged"));
    // No `id` is the recovery mechanism: the client keeps the last seq it really has and asks to
    // resume after that, instead of resuming past events it never saw.
    assert!(
        last.id.is_none(),
        "the lag frame carried an id, so the client would resume past the gap: {last:?}"
    );
    let body: serde_json::Value = serde_json::from_str(&last.data).expect("the payload is JSON");
    assert!(
        body["skipped"].as_u64().unwrap_or(0) > 0,
        "the lag report named no skipped events: {}",
        last.data
    );

    // A stream that ended by lagging releases its registration like any other: must not be
    // told a browser is still attached to a session it dropped.
    let session_id = session.id.clone();
    wait_until("the lagged stream releases its count", || {
        state.streams.count(&session_id) == 0
    })
    .await;

    drop(slow);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn a_reconnect_after_a_lag_refills_the_gap() {
    let dir = TempDir::new("streams-reconnect-after-lag");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "reconnect-after-lag");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut slow = Sse::connect(served.addr, &session.id, None).await;
    let (lagged, total) = flood_until_lagged(&state, &session, &mut slow).await;
    assert_eq!(lagged.kind.as_deref(), Some("stream_lagged"));
    assert!(
        lagged.id.is_none(),
        "the lag frame carried an id: {lagged:?}"
    );

    // The lag frame carried no `id`, so the client still holds the last seq it really saw. It
    // reconnects from inside the gap and the replay rebuilds the rest from the database.
    let resume = total / 2;
    let mut again = Sse::connect(served.addr, &session.id, Some(resume)).await;
    let frames = again.take(3, PATIENCE).await;
    assert_eq!(
        frames.iter().map(Frame::seq).collect::<Vec<i64>>(),
        [resume + 1, resume + 2, resume + 3],
        "the reconnect did not replay the gap the lag left"
    );

    drop(slow);
    drop(again);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn the_keep_alive_is_fifteen_seconds() {
    assert_eq!(
        holosmith::http::KEEP_ALIVE_INTERVAL,
        Duration::from_secs(15),
        "the keep-alive is axum's KeepAlive interval, and the spec says 15 s"
    );

    let dir = TempDir::new("streams-keep-alive");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "keep-alive");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // Nothing is written before that interval: no comment line of our own doubling the
    // keep-alive, and no empty event.
    let mut stream = Sse::connect(served.addr, &session.id, None).await;
    assert!(
        stream.quiet_for(Duration::from_millis(300)).await,
        "the stream wrote something before the 15 s keep-alive"
    );

    drop(stream);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn closing_a_stream_ends_it_without_a_panic() {
    let dir = TempDir::new("streams-close");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "close");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut stream = Sse::connect(served.addr, &session.id, None).await;
    append(&state, &session, "session.status").await;
    let _ = stream.take(1, PATIENCE).await;

    // What the delete flow does: the stream ends cleanly, at a block boundary.
    assert_eq!(state.streams.close(&session.id), 1);
    assert!(
        stream.next_block(PATIENCE).await.is_none(),
        "the stream did not end when the session closed"
    );

    // And the process is still serving: a closed stream is not a panic.
    let health = support::fetch(served.addr, "/api/health").await;
    assert_eq!(health.status, 200);

    drop(stream);
    served.finish().await.expect("the app stops");
}

#[test]
fn the_registry_counts_open_streams() {
    let registry = holosmith::events::StreamRegistry::new();
    let guard = registry.open("0123456789abcdef");
    assert_eq!(registry.count("0123456789abcdef"), 1);
    assert_eq!(registry.count("fedcba9876543210"), 0);

    drop(guard);
    assert_eq!(
        registry.count("0123456789abcdef"),
        0,
        "the count outlived its guard"
    );
}

#[tokio::test]
async fn the_registry_releases_a_closed_stream() {
    let dir = TempDir::new("streams-registry");
    let state = support::state_in(&dir, Config::default());
    let session = create_session(&state, "registry");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut stream = Sse::connect(served.addr, &session.id, None).await;
    // Registered by the handler, not by this test: the count is the "SSE open on this session"
    // registration reads.
    let session_id = session.id.clone();
    let registry = state.streams.clone();
    wait_until("the stream registers itself", || {
        registry.count(&session_id) == 1
    })
    .await;

    state.streams.close(&session.id);
    wait_until("the closed stream releases its count", || {
        registry.count(&session_id) == 0
    })
    .await;
    assert_eq!(
        registry.count(&session_id),
        0,
        "the count outlived its stream"
    );
    assert!(stream.next_block(PATIENCE).await.is_none());

    drop(stream);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn an_event_committed_during_the_replay_window_is_neither_lost_nor_duplicated() {
    // A long commit window is what makes the window reachable: the append below is queued
    // before the stream attaches and commits after its history read, so it reaches the client
    // through the live half alone. A stream that read history before subscribing would have
    // dropped it.
    let dir = TempDir::new("streams-replay-window");
    let state = support::state_with_writer(
        &dir,
        Config::default(),
        WriterOptions {
            commit_interval: Duration::from_millis(500),
        },
    );
    let session = create_session(&state, "replay-window");
    append(&state, &session, "first").await;
    wait_until("the first event commits", || {
        committed(&state, &session.id) == 1
    })
    .await;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    append(&state, &session, "second").await;
    let mut stream = Sse::connect(served.addr, &session.id, None).await;
    let frames = stream.take(2, PATIENCE).await;
    assert_eq!(
        frames.iter().map(Frame::seq).collect::<Vec<i64>>(),
        [1, 2],
        "an event committed inside the replay window was lost or duplicated"
    );
    assert_eq!(
        frames[0].kind.as_deref(),
        Some("first"),
        "the replay and the live half arrived out of order"
    );
    assert_eq!(frames[1].kind.as_deref(), Some("second"));

    drop(stream);
    served.finish().await.expect("the app stops");
}

#[tokio::test]
async fn an_unknown_session_is_404_not_found() {
    let dir = TempDir::new("streams-unknown");
    let state = support::state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let response = support::fetch(served.addr, "/api/sessions/0123456789abcdef/events").await;
    assert_eq!(response.status, 404);
    let body: serde_json::Value =
        serde_json::from_str(&response.body).expect("the answer is the JSON envelope");
    assert_eq!(body["code"].as_str(), Some("not_found"));
    assert_eq!(
        state.streams.count("0123456789abcdef"),
        0,
        "an unknown session registered a stream"
    );

    served.finish().await.expect("the app stops");
}
