//! The stream's truncation and `GET /api/sessions/:id/events/history` (HS-061, spec item 15).
//!
//! The story's five acceptance lines, one test each, over a real socket. The log is written by hand
//! rather than by the stub's turn: the rule under test is about *messages*, and a script that emitted
//! three messages in one turn would test the same thing with a longer setup.
//!
//! A window is set through the config, because the story says the window is configurable and that is
//! the only place a window may live (ADR-0006). Nothing here waits on a row count: coalescing means
//! "one message" is not "one row" (ADR-0031), so the tests wait for the log to hold the rows they wrote
//! and then read the seqs out of it.

use std::time::{Duration, Instant};

use holosmith::clock;
use holosmith::config::Config;
use holosmith::events::{Event, MESSAGE_DELTA, MESSAGE_END, NewEvent, TURN_END, TURN_START};
use holosmith::http::{self, AppState};
use holosmith::sessions::{NewSession, Session};

mod support;
use support::{PATIENCE, Served, Sse, TempDir};

/// The drain deadline each test gives `serve`, so a socket a test forgot to drop cannot hold the
/// server open for the production ten seconds.
const GRACE: Duration = Duration::from_millis(100);

/// The default config with one setting changed: the window this story made configurable.
fn config(window: u32) -> Config {
    Config {
        history_window: window,
        ..Config::default()
    }
}

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

async fn append(state: &AppState, session: &Session, kind: &str, payload: String) {
    state
        .events
        .append(NewEvent {
            session_id: session.id.clone(),
            kind: kind.to_string(),
            payload,
            created_at: clock::unix_now(),
        })
        .await
        .expect("the writer accepts the event");
}

/// One turn's four frames: a boundary, one message's delta, its end, and the closing boundary.
async fn turn(state: &AppState, session: &Session, message: &str, text: &str) {
    append(state, session, TURN_START, "{}".to_string()).await;
    append(
        state,
        session,
        MESSAGE_DELTA,
        format!("{{\"message_id\":\"{message}\",\"kind\":\"text\",\"text\":\"{text}\"}}"),
    )
    .await;
    append(
        state,
        session,
        MESSAGE_END,
        format!("{{\"message_id\":\"{message}\",\"usage\":{{\"in\":1,\"out\":1}}}}"),
    )
    .await;
    append(state, session, TURN_END, "{}".to_string()).await;
}

/// Every committed row of one session, in seq order.
fn log(state: &AppState, session_id: &str) -> Vec<Event> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session_id, 0, u32::MAX)
        .expect("the log reads")
}

/// Wait until the session's log holds at least `rows` rows, and answer it.
///
/// A count is only a race when coalescing can merge two of the rows a test wrote; these frames are
/// four different kinds per turn, so the number is a fact once the writer has committed.
async fn committed(state: &AppState, session_id: &str, rows: usize) -> Vec<Event> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let log = log(state, session_id);
        if log.len() >= rows {
            return log;
        }
        assert!(
            Instant::now() < deadline,
            "the log did not reach {rows} rows within {PATIENCE:?}: {log:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The lowest seq the message `message` occupies: the first row of its block.
fn first_seq_of(log: &[Event], message: &str) -> i64 {
    let needle = format!("\"message_id\":\"{message}\"");
    log.iter()
        .find(|event| event.payload.contains(&needle))
        .map(|event| event.seq)
        .unwrap_or_else(|| panic!("{message} is not in the log: {log:?}"))
}

/// The seqs a page holds, in the order it holds them.
fn seqs(body: &str) -> Vec<i64> {
    let page: serde_json::Value = serde_json::from_str(body).expect("the page is JSON");
    page["events"]
        .as_array()
        .expect("the page holds an events array")
        .iter()
        .map(|event| event["seq"].as_i64().expect("every event carries a seq"))
        .collect()
}

fn page(body: &str) -> serde_json::Value {
    serde_json::from_str(body).expect("the page is JSON")
}

/// Three turns, which is one message more than a window of two: the smallest log that truncates.
async fn three_turns(state: &AppState, session: &Session) -> Vec<Event> {
    for (message, text) in [("m1", "one"), ("m2", "two"), ("m3", "three")] {
        turn(state, session, message, text).await;
    }
    committed(state, &session.id, 12).await
}

#[tokio::test]
async fn a_new_client_gets_the_windows_oldest_message_and_a_truncation_frame() {
    let dir = TempDir::new("history-truncated");
    let state = support::state_in(&dir, config(2));
    let session = create_session(&state, "truncated");
    let log = three_turns(&state, &session).await;
    let cut = first_seq_of(&log, "m2");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // No `Last-Event-ID`: this client has nothing, which is the case the story is about.
    let mut stream = Sse::connect(served.addr, &session.id, None).await;
    let frame = stream
        .next_block(PATIENCE)
        .await
        .expect("the stream sent something");
    assert_eq!(frame.kind.as_deref(), Some("history.truncated"));
    // No `id`, for the lag report's reason: a browser would resume after a seq that is not a row.
    assert!(
        frame.id.is_none(),
        "the truncation frame carried an id, so a client would resume past a seq the log does not have: {frame:?}"
    );
    let announced = page(&frame.data);
    assert_eq!(
        announced["before_seq"].as_i64(),
        Some(cut),
        "the frame did not name the window's oldest message's first row"
    );
    assert_eq!(
        announced["turns_before"].as_u64(),
        Some(2),
        "the frame did not say how many turn boundaries the client never saw"
    );

    // The replay begins exactly where the frame said it would, so the two halves of the rule agree.
    let replay = stream.take(4, PATIENCE).await;
    assert_eq!(replay[0].seq(), cut);
}

#[tokio::test]
async fn a_log_that_fits_the_window_is_replayed_from_its_first_row() {
    let dir = TempDir::new("history-fits");
    let state = support::state_in(&dir, config(2));
    let session = create_session(&state, "fits");
    turn(&state, &session, "m1", "one").await;
    turn(&state, &session, "m2", "two").await;
    committed(&state, &session.id, 8).await;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let mut stream = Sse::connect(served.addr, &session.id, None).await;
    let first = stream
        .next_block(PATIENCE)
        .await
        .expect("the stream sent something");
    // A log of exactly the window's worth of messages has a cut at its own first row, so there is
    // nothing to announce and nothing to load earlier.
    assert_eq!(
        first.kind.as_deref(),
        Some(TURN_START),
        "a log that fits the window was truncated: {first:?}"
    );
    assert_eq!(first.seq(), 1);
}

#[tokio::test]
async fn a_client_with_a_resume_point_is_never_truncated() {
    let dir = TempDir::new("history-resume");
    let state = support::state_in(&dir, config(2));
    let session = create_session(&state, "resume");
    three_turns(&state, &session).await;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // A reconnecting client already has everything below its own point, so cutting under it would throw
    // away events it is entitled to. The window is not a factor here.
    let mut stream = Sse::connect(served.addr, &session.id, Some(4)).await;
    let first = stream
        .next_block(PATIENCE)
        .await
        .expect("the stream sent something");
    assert_eq!(first.kind.as_deref(), Some(TURN_START));
    assert_eq!(
        first.seq(),
        5,
        "a resumed replay did not continue after the client's point"
    );
}

#[tokio::test]
async fn a_page_returns_the_events_below_the_cursor_in_order() {
    let dir = TempDir::new("history-page");
    let state = support::state_in(&dir, config(2));
    let session = create_session(&state, "page");
    let log = three_turns(&state, &session).await;
    let cut = first_seq_of(&log, "m2");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let answer = support::fetch(
        served.addr,
        &format!("/api/sessions/{}/events/history?before={cut}", session.id),
    )
    .await;
    assert_eq!(answer.status, 200);
    assert_eq!(
        answer.header("content-type"),
        Some("application/json"),
        "the page did not answer the envelope's content type"
    );
    // Everything below the cut, in seq order: m1's whole turn and the boundary that introduced m2 —
    // which the replay above the cut therefore did not carry.
    assert_eq!(seqs(&answer.body), (1..cut).collect::<Vec<i64>>());
    let body = page(&answer.body);
    assert_eq!(body["before_seq"].as_i64(), Some(1));
    assert_eq!(body["has_more"].as_bool(), Some(false));
    // The page begins at the log's first row, so no turn boundary sits below it.
    assert_eq!(body["turns_before"].as_u64(), Some(0));
}

#[tokio::test]
async fn a_full_page_says_there_is_more_and_the_last_page_ends_it() {
    let dir = TempDir::new("history-more");
    let state = support::state_in(&dir, config(2));
    let session = create_session(&state, "more");
    let log = three_turns(&state, &session).await;
    let cut = first_seq_of(&log, "m2");
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // Two rows at a time, following only the cursor each page names: the walk must reach the first row
    // and must not repeat one, which is what "a page's content does not shift" buys a client.
    let mut cursor = cut;
    let mut walked: Vec<i64> = Vec::new();
    loop {
        let answer = support::fetch(
            served.addr,
            &format!(
                "/api/sessions/{}/events/history?before={cursor}&limit=2",
                session.id
            ),
        )
        .await;
        assert_eq!(answer.status, 200);
        let body = page(&answer.body);
        let page_seqs = seqs(&answer.body);
        assert!(!page_seqs.is_empty(), "a page before {cursor} was empty");
        assert!(
            page_seqs.iter().all(|seq| *seq < cursor),
            "a page held a row at or above its own cursor: {page_seqs:?} for {cursor}"
        );
        let next = body["before_seq"]
            .as_i64()
            .expect("a page names its cursor");
        let more = body["has_more"]
            .as_bool()
            .expect("a page says whether it is full");
        // Only a full page has more: a page of one row at the end cannot.
        assert_eq!(
            more,
            page_seqs.len() == 2,
            "{page_seqs:?} claimed has_more {more}"
        );
        walked.splice(0..0, page_seqs);
        if !more {
            break;
        }
        assert!(next < cursor, "the cursor did not move down");
        cursor = next;
    }
    assert_eq!(
        walked,
        (1..cut).collect::<Vec<i64>>(),
        "the pages did not add up to the log below the cut, exactly once each"
    );
}

#[tokio::test]
async fn a_page_does_not_shift_when_events_arrive_above_it() {
    let dir = TempDir::new("history-stable");
    let state = support::state_in(&dir, config(2));
    let session = create_session(&state, "stable");
    let log = three_turns(&state, &session).await;
    let cut = first_seq_of(&log, "m2");
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let path = format!(
        "/api/sessions/{}/events/history?before={cut}&limit=3",
        session.id
    );

    let first = support::fetch(served.addr, &path).await;
    assert_eq!(first.status, 200);
    // A new turn arrives above the cursor while the client is paging. Seq is strictly increasing, so it
    // can only land above every row a page below the cursor could hold.
    turn(&state, &session, "m4", "four").await;
    committed(&state, &session.id, 16).await;

    let again = support::fetch(served.addr, &path).await;
    assert_eq!(
        first.body, again.body,
        "a page's content moved because new events arrived above it"
    );
}

#[tokio::test]
async fn a_bad_cursor_is_refused_with_the_envelope_and_an_unknown_session_is_not_found() {
    let dir = TempDir::new("history-refused");
    let state = support::state_in(&dir, config(2));
    let session = create_session(&state, "refused");
    three_turns(&state, &session).await;
    let served = Served::start(http::router(state.clone()), GRACE).await;

    for query in ["", "?before=0", "?before=abc", "?before=-2", "?limit=5"] {
        let answer = support::fetch(
            served.addr,
            &format!("/api/sessions/{}/events/history{query}", session.id),
        )
        .await;
        assert_eq!(answer.status, 422, "query {query:?} was not refused");
        assert_eq!(
            answer.header("content-type"),
            Some("application/json"),
            "query {query:?} answered outside the envelope"
        );
        let body = page(&answer.body);
        assert_eq!(body["code"].as_str(), Some("bad_query"), "query {query:?}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "query {query:?} answered no sentence"
        );
    }

    let unknown = support::fetch(
        served.addr,
        "/api/sessions/ffffffffffffffff/events/history?before=3",
    )
    .await;
    assert_eq!(unknown.status, 404);
    assert_eq!(page(&unknown.body)["code"].as_str(), Some("not_found"));
}
