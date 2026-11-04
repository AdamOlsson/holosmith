//! One prompt, one turn: `POST /api/sessions/{id}/prompt` answers 202 with the now-`running` row,
//! the stub's script arrives as events, `agent_settled` takes the session back to `idle`, and
//! `last_active_at` moves at both ends (HS-015).
//!
//! Every refusable state is produced the way the server produces it: `starting` by prompting a
//! session the slow stub has not finished provisioning, `running` by prompting twice, and `stopped`
//! and `error` by driving the one state function directly — there is no endpoint for either yet
//! (HS-018 and HS-019 own them). The turn itself is read off the wire, because the stream is what a
//! browser sees.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::http::Method;
use holosmith::backend::StubBackend;
use holosmith::config::Config;
use holosmith::http::error::{
    BAD_BODY, NOT_FOUND, SESSION_ERROR, SESSION_RUNNING, SESSION_STARTING, SESSION_STOPPED,
};
use holosmith::http::{self, AppState, Guards, RouteKind};
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::sessions::{Session, Status};
use serde_json::Value;

mod support;
use support::{
    PATIENCE, Response, Served, Sse, TempDir, fast_stub, request, state, state_in, with_stub,
};

/// The drain deadline a test gives `serve`: a socket it forgot to drop cannot hold the server for
/// the production ten seconds.
const GRACE: Duration = Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

const PROMPT: &str = r#"{"prompt":"read the app shell"}"#;

/// A stub that spends `millis` on each provisioning step *and* on each frame of a turn, so a test can
/// watch a turn happen. A hundred milliseconds is the smallest pace that also guarantees the turn
/// spans a whole second, which is what `last_active_at`'s second resolution needs to see it move.
fn paced(millis: u64) -> StubBackend {
    StubBackend::new(Duration::from_millis(millis))
}

/// One create, the way the app's own client sends it.
async fn post_create(addr: SocketAddr, key: &str) -> Response {
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nIdempotency-Key: {key}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    );
    request(addr, &text).await
}

/// One prompt, with the headers the guards require on a request that changes something.
async fn post_prompt(addr: SocketAddr, id: &str, body: &str) -> Response {
    let text = format!(
        "POST /api/sessions/{id}/prompt HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    request(addr, &text).await
}

/// The `id` a create answered with, so a test can name the session it just made.
fn created_id(response: &Response) -> String {
    assert_eq!(response.status, 202, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("a create answers JSON");
    body["id"]
        .as_str()
        .expect("the response names an id")
        .to_string()
}

/// A response that must be the ADR-0008 envelope for `code`: status, `content-type` and code, all
/// three, because a status-only assertion is how the wrong code ships (HS-005's review).
fn assert_envelope(response: &Response, status: u16, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let body: Value = serde_json::from_str(&response.body).expect("an envelope is JSON");
    assert_eq!(body["code"], code, "{}", response.body);
}

fn row(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

/// Poll `condition` — a state the server reaches on its own — until it holds.
async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not happen within {PATIENCE:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Wait until the session is `idle`, which for a fast stub is its provisioned state and for a paced
/// one is the end of the turn.
async fn wait_idle(state: &AppState, id: &str) {
    wait_until("the session reached idle", || {
        row(state, id).status == Status::Idle
    })
    .await;
}

/// Move a session with the one state function, the way the stories that own those moves will:
/// `stopped` needs HS-018's endpoint and `error` needs a failed turn or a dead container.
async fn drive(state: &AppState, id: &str, trigger: Trigger) {
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, None)
        .await
        .expect("the move is in the table");
}

// ---- the accepted prompt -------------------------------------------------------------

#[tokio::test]
async fn a_prompt_answers_the_running_row_and_a_second_prompt_is_refused() {
    let dir = TempDir::new("turns-accepted");
    let state = with_stub(state_in(&dir, Config::default()), paced(100));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "turns-accepted").await);
    wait_idle(&state, &id).await;

    let accepted = post_prompt(served.addr, &id, PROMPT).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    assert_eq!(
        accepted.header("content-type"),
        Some("application/json"),
        "{}",
        accepted.body
    );
    let body: Value = serde_json::from_str(&accepted.body).expect("the answer is JSON");
    // The answer is the row, now `running`: D4's rule, and what a client refetches after a 202.
    assert_eq!(body["status"], "running", "{}", accepted.body);
    assert_eq!(body["id"], id.as_str());
    assert!(
        !body.as_object().expect("one object").contains_key("events"),
        "one row carries no events key: {}",
        accepted.body
    );

    // A second prompt while that turn runs: the session is not idle, and the code names the state.
    let refused = post_prompt(served.addr, &id, PROMPT).await;
    assert_envelope(&refused, 409, SESSION_RUNNING);

    // The stub's script settles the turn on its own, and nothing else moves the session.
    wait_idle(&state, &id).await;
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_turn_settles_the_session_and_moves_last_active_at() {
    let dir = TempDir::new("turns-activity");
    let state = with_stub(state_in(&dir, Config::default()), paced(100));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "turns-activity").await);
    wait_idle(&state, &id).await;
    let before = row(&state, &id).last_active_at;

    let accepted = post_prompt(served.addr, &id, PROMPT).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    wait_idle(&state, &id).await;

    let after = row(&state, &id);
    assert_eq!(after.status, Status::Idle, "agent_settled is the way out");
    assert_eq!(after.status_reason, None);
    // Strictly increasing: the runner touches at the prompt and again at the settle, and a paced turn
    // spans at least one second, so a resolution of one second can see both.
    assert!(
        after.last_active_at > before,
        "last_active_at did not move: {before} -> {}",
        after.last_active_at
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_stream_carries_the_scripts_six_kinds_in_order_and_the_turn_settles() {
    let dir = TempDir::new("turns-stream");
    let state = with_stub(state_in(&dir, Config::default()), paced(30));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "turns-stream").await);
    wait_idle(&state, &id).await;

    let accepted = post_prompt(served.addr, &id, PROMPT).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);

    // The whole stream from its first seq: the provisioning of this session, then the turn.
    let mut stream = Sse::connect(served.addr, &id, None).await;
    assert_eq!(stream.status, 200, "the stream opens");
    let mut frames = Vec::new();
    let settled = loop {
        let frame = stream.next_block(PATIENCE).await.expect("the stream runs");
        let is_settle = frame.kind.as_deref() == Some("session.status")
            && frame.data.contains(r#""from":"running","to":"idle""#);
        frames.push(frame);
        if is_settle {
            break frames.len() - 1;
        }
        assert!(
            frames.len() < 64,
            "the turn never settled: {} frames",
            frames.len()
        );
    };

    let start = frames
        .iter()
        .position(|frame| frame.kind.as_deref() == Some("turn.start"))
        .expect("the turn's start is an event of its own");
    // The row moved before the first frame of the turn: the status event that says `running` comes
    // before `turn.start` (D4).
    let running = frames
        .iter()
        .position(|frame| {
            frame.kind.as_deref() == Some("session.status")
                && frame.data.contains(r#""from":"idle","to":"running""#)
        })
        .expect("the prompt's transition is in the stream");
    assert!(running < start, "the turn started before the row moved");

    // The six kinds, in the order the script emits them, with repeats collapsed.
    let mut kinds: Vec<&str> = Vec::new();
    for frame in &frames[start..] {
        let kind = frame.kind.as_deref().expect("every frame names its kind");
        if kinds.last() != Some(&kind) {
            kinds.push(kind);
        }
    }
    assert_eq!(
        kinds,
        [
            "turn.start",
            "message.delta",
            "tool.start",
            "tool.end",
            "message.delta",
            "message.end",
            "turn.end",
            "session.status",
        ],
        "the turn's kinds, in order"
    );
    assert_eq!(settled, frames.len() - 1, "the settle is the last frame");

    // One message id for the turn, one tool id for its tool call, and the usage on the message.
    let payload = |index: usize| -> Value {
        serde_json::from_str(&frames[index].data).expect("a payload is JSON")
    };
    let message_id = |index: usize| -> String {
        payload(index)["message_id"]
            .as_str()
            .expect("a delta names its message")
            .to_string()
    };
    let turn = &frames[start..settled];
    let deltas: Vec<usize> = (0..turn.len())
        .filter(|index| turn[*index].kind.as_deref() == Some("message.delta"))
        .collect();
    assert_eq!(deltas.len(), 7, "three thinking, two text, two text again");
    let first = message_id(start + deltas[0]);
    assert!(
        first.starts_with("msg-"),
        "the stub mints `msg-<n>`: {first}"
    );
    for index in &deltas {
        assert_eq!(message_id(start + index), first, "one id per turn");
    }
    let first_thinking = payload(start + deltas[0]);
    assert_eq!(
        first_thinking["kind"], "thinking",
        "the script thinks first"
    );
    let tool_start = frames
        .iter()
        .position(|frame| frame.kind.as_deref() == Some("tool.start"))
        .expect("a tool call starts");
    let tool_end = frames
        .iter()
        .position(|frame| frame.kind.as_deref() == Some("tool.end"))
        .expect("a tool call ends");
    assert_eq!(
        payload(tool_start)["tool_id"],
        payload(tool_end)["tool_id"],
        "the tool's end names the call its start made"
    );
    assert_eq!(payload(tool_start)["name"], "read_file");
    assert_eq!(payload(tool_end)["name"], "read_file");
    let ended = frames
        .iter()
        .position(|frame| frame.kind.as_deref() == Some("message.end"))
        .expect("a message ends");
    assert_eq!(payload(ended)["message_id"], first.as_str());
    assert_eq!(payload(ended)["usage"]["in"], 1200);
    assert_eq!(payload(ended)["usage"]["out"], 340);

    served.finish().await.expect("a clean shutdown");
}

// ---- the refusals --------------------------------------------------------------------

#[tokio::test]
async fn a_prompt_while_the_session_is_starting_names_that_state() {
    // The harness's slow stub keeps the session `starting` while this test looks, so nothing has to
    // be raced to see the state.
    let dir = TempDir::new("turns-starting");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "turns-starting").await);
    assert_eq!(row(&state, &id).status, Status::Starting);

    let refused = post_prompt(served.addr, &id, PROMPT).await;
    assert_envelope(&refused, 409, SESSION_STARTING);
    assert_eq!(
        row(&state, &id).status,
        Status::Starting,
        "a refusal changes nothing"
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_prompt_on_a_stopped_session_is_refused_with_session_stopped() {
    let dir = TempDir::new("turns-stopped");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "turns-stopped").await);
    wait_idle(&state, &id).await;
    drive(&state, &id, Trigger::Evict).await;

    let refused = post_prompt(served.addr, &id, PROMPT).await;
    assert_envelope(&refused, 409, SESSION_STOPPED);
    assert_eq!(row(&state, &id).status, Status::Stopped);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_prompt_on_a_failed_session_is_refused_with_session_error() {
    let dir = TempDir::new("turns-error");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "turns-error").await);
    wait_idle(&state, &id).await;
    // `idle + unexpected_exit` is the table's row for a session that lost its container (spec item
    // 6). The reason is the operator's explanation, so it rides along.
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(&id, Trigger::UnexpectedExit, Some("the container exited"))
        .await
        .expect("the move is in the table");

    let refused = post_prompt(served.addr, &id, PROMPT).await;
    assert_envelope(&refused, 409, SESSION_ERROR);
    assert_eq!(row(&state, &id).status, Status::Error);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_prompt_for_an_unknown_session_is_not_found() {
    let dir = TempDir::new("turns-missing");
    let served = Served::start(http::router(state_in(&dir, Config::default())), GRACE).await;
    let refused = post_prompt(served.addr, "0000000000000000", PROMPT).await;
    assert_envelope(&refused, 404, NOT_FOUND);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_prompt_body_of_the_wrong_shape_is_bad_body_before_anything_else() {
    let dir = TempDir::new("turns-bad-body");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "turns-bad-body").await);
    wait_idle(&state, &id).await;

    for body in [
        "{}",
        r#"{"prompt":""}"#,
        r#"{"prompt":42}"#,
        "[]",
        r#"{"prompt":"p","extra":1}"#,
    ] {
        let refused = post_prompt(served.addr, &id, body).await;
        assert_envelope(&refused, 422, BAD_BODY);
    }
    // The body is decided first, so an unknown session with a bad body is still `bad_body`: the row
    // is never read for a request that cannot be a prompt.
    let refused = post_prompt(served.addr, "0000000000000000", "not json").await;
    assert_envelope(&refused, 422, BAD_BODY);
    assert_eq!(
        row(&state, &id).status,
        Status::Idle,
        "no refusal started a turn"
    );
    served.finish().await.expect("a clean shutdown");
}

#[test]
fn the_prompt_route_is_recorded_mutating() {
    // Not observable from outside, so it is read from the manifest `Routes::post` fills: a prompt
    // changes a session's state, so it must pass every guard and be recorded as mutating.
    let app = http::App::new(
        http::router(state()),
        Guards::from_config(&Config::default()),
    );
    let entry = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}/prompt" && entry.method == Method::POST)
        .expect("the prompt is a registered route");
    assert_eq!(entry.kind, RouteKind::Mutating);
}

#[tokio::test]
async fn a_burst_of_deltas_is_fewer_rows_than_frames_and_keeps_its_text() {
    // HS-059 on the live path: the stub's script emits seven deltas, and with a pace far below the writer's
    // 25 ms window they arrive inside a couple of commits — so the log holds the script's three *runs*
    // (thinking, then text, then text after the tool call) rather than seven rows, and the words are all
    // still there in the order they were said.
    let dir = TempDir::new("turns-coalescing");
    let state = with_stub(state_in(&dir, Config::default()), paced(0));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "turns-coalescing").await);
    wait_idle(&state, &id).await;

    let accepted = post_prompt(served.addr, &id, PROMPT).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    wait_idle(&state, &id).await;
    // The row can be ahead of the writer's commit (ADR-0013), so wait for the turn's last event.
    wait_until("the turn's frames are committed", || {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .events()
            .read_since(&id, 0, 100)
            .expect("the log reads")
            .iter()
            .any(|event| event.kind == "turn.end")
    })
    .await;

    let events: Vec<(String, String)> = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .events()
            .read_since(&id, 0, 100)
            .expect("the log reads")
            .into_iter()
            .map(|event| (event.kind, event.payload))
            .collect()
    };
    let payload = |text: &str| -> serde_json::Value {
        serde_json::from_str(text).expect("a payload is JSON")
    };
    // Coalescing is *within a commit*, so two rows of one message and kind may still sit next to each
    // other across a batch boundary (the script's answer is 200 ms of thought and then two sentences, and a
    // batch can end between them). What must never happen is two **consecutive events** that the rule would
    // have merged: that would be a batch that failed to coalesce.
    for pair in events.windows(2) {
        if pair[0].0 != "message.delta" || pair[1].0 != "message.delta" {
            continue;
        }
        let (one, two) = (payload(&pair[0].1), payload(&pair[1].1));
        assert!(
            one["message_id"] != two["message_id"] || one["kind"] != two["kind"],
            "two consecutive events are deltas of one message and kind: {pair:?}"
        );
    }
    let deltas: Vec<(String, String, String)> = events
        .iter()
        .filter(|(kind, _)| kind == "message.delta")
        .map(|(_, text)| {
            let payload = payload(text);
            (
                payload["message_id"]
                    .as_str()
                    .expect("a delta names its message")
                    .to_string(),
                payload["kind"]
                    .as_str()
                    .expect("a delta names its kind")
                    .to_string(),
                payload["text"]
                    .as_str()
                    .expect("a delta carries its text")
                    .to_string(),
            )
        })
        .collect();
    assert!(
        deltas.len() < 7,
        "seven scripted deltas are {} rows, so nothing coalesced",
        deltas.len()
    );
    assert_eq!(
        deltas.len(),
        3,
        "the script's three runs are not three rows: {deltas:#?}"
    );
    assert!(
        deltas.iter().all(|(id, _, _)| *id == deltas[0].0),
        "one turn is one message id: {deltas:#?}"
    );
    assert_eq!(
        deltas
            .iter()
            .map(|(_, kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        ["thinking", "text", "text"],
        "the runs are the script's three"
    );
    // The first and last things the script says survived the merge, in order.
    assert!(
        deltas[0].2.starts_with("The prompt names the app shell."),
        "the first thinking sentence was lost: {:?}",
        deltas[0].2
    );
    assert!(
        deltas[2].2.ends_with("Nothing else in it moved."),
        "the last text sentence was lost: {:?}",
        deltas[2].2
    );
    served.finish().await.expect("a clean shutdown");
}
