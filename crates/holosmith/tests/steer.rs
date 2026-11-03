//! Steer (HS-017): `POST /api/sessions/{id}/steer` hands guidance to the running turn, answers 202
//! with the row, and records what was delivered as a `turn.steer` frame so the transcript can show
//! it. A steer changes no state — the table has no row for it — so the event and `last_active_at` are
//! the whole of its effect on the session.
//!
//! Every state a refusal needs is produced the way the server produces it: `idle` by letting a fast
//! stub provision, `running` by prompting, and `stopped` by driving the table's `idle_stop` row,
//! because HS-018's endpoint does not exist yet.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::Method;
use holosmith::backend::{Call, StubBackend};
use holosmith::config::Config;
use holosmith::http::TURN_STEER;
use holosmith::http::error::{BAD_BODY, NOT_FOUND, SESSION_IDLE, SESSION_STOPPED};
use holosmith::http::{self, AppState, Guards, RouteKind};
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::sessions::{Session, Status};
use serde_json::Value;

mod support;
use support::{
    PATIENCE, Response, Served, TempDir, fast_stub, request, state, state_in, with_stub,
};

/// The drain deadline a test gives `serve`.
const GRACE: Duration = Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

const PROMPT: &str = r#"{"prompt":"read the app shell"}"#;

const GUIDANCE: &str = "and the lexer too";

/// A stub that spends `millis` on each provisioning step and on each frame of a turn, so a turn is
/// still running when the steer arrives.
fn paced(millis: u64) -> StubBackend {
    StubBackend::new(Duration::from_millis(millis))
}

async fn post_create(addr: SocketAddr, key: &str) -> Response {
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nIdempotency-Key: {key}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    );
    request(addr, &text).await
}

async fn post_prompt(addr: SocketAddr, id: &str) -> Response {
    let text = format!(
        "POST /api/sessions/{id}/prompt HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{PROMPT}",
        PROMPT.len()
    );
    request(addr, &text).await
}

/// One steer, with whatever body the test wants.
async fn post_steer(addr: SocketAddr, id: &str, body: &str) -> Response {
    let text = format!(
        "POST /api/sessions/{id}/steer HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    request(addr, &text).await
}

fn created_id(response: &Response) -> String {
    assert_eq!(response.status, 202, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("a create answers JSON");
    body["id"]
        .as_str()
        .expect("the response names an id")
        .to_string()
}

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

/// Every committed event of `session` as `(kind, payload)`, in seq order.
fn events(state: &AppState, session: &str) -> Vec<(String, Value)> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session, 0, 200)
        .expect("the events read")
        .into_iter()
        .map(|event| {
            (
                event.kind,
                serde_json::from_str(&event.payload).expect("a payload is JSON"),
            )
        })
        .collect()
}

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

async fn wait_idle(state: &AppState, id: &str) {
    wait_until("the session reached idle", || {
        row(state, id).status == Status::Idle
    })
    .await;
}

/// Wait for a turn to be running, so a steer has something to steer.
async fn wait_running(state: &AppState, id: &str) {
    wait_until("the turn is running", || {
        row(state, id).status == Status::Running
    })
    .await;
}

/// Move a session with the one state function, the way HS-018's endpoint will: `stopped` is
/// reachable from `idle` by the table's `idle_stop` row.
async fn drive(state: &AppState, id: &str, trigger: Trigger) {
    Transitioner::new(std::sync::Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, None)
        .await
        .expect("the move is in the table");
}

#[tokio::test]
async fn a_steer_answers_the_running_row_and_records_what_was_delivered() {
    let dir = TempDir::new("steer-accepted");
    let stub = paced(100);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "steer-accepted").await);
    wait_idle(&state, &id).await;
    assert_eq!(
        post_prompt(served.addr, &id).await.status,
        202,
        "the prompt is accepted"
    );
    wait_running(&state, &id).await;
    let before = row(&state, &id).last_active_at;

    let steered = post_steer(served.addr, &id, &format!(r#"{{"guidance":"{GUIDANCE}"}}"#)).await;
    assert_eq!(steered.status, 202, "{}", steered.body);
    assert_eq!(
        steered.header("content-type"),
        Some("application/json"),
        "{}",
        steered.body
    );
    let body: Value = serde_json::from_str(&steered.body).expect("the answer is JSON");
    // The answer is the row, still `running`: a steer changes no state, and the transition table has
    // no row for one.
    assert_eq!(body["status"], "running", "{}", steered.body);
    assert_eq!(body["id"], id.as_str());

    // The guidance reached the agent, and the transcript's frame says what it was.
    assert_eq!(
        stub.calls().last().copied(),
        Some(Call::Steer),
        "the backend was not asked to steer: {:?}",
        stub.calls()
    );
    let delivered = stub
        .recorded()
        .into_iter()
        .find(|entry| entry.call == Call::Steer)
        .expect("the steer was recorded");
    assert_eq!(delivered.text, GUIDANCE, "the guidance was not delivered");

    wait_until("the steer's event is committed", || {
        events(&state, &id)
            .iter()
            .any(|(kind, payload)| kind == TURN_STEER && payload["text"] == GUIDANCE)
    })
    .await;
    // HS-015's acceptance: `last_active_at` moves on prompt, settle and steer.
    wait_until("last_active_at moved", || {
        row(&state, &id).last_active_at > before
    })
    .await;

    // The turn continues: the guide did not end it, and nothing else moved the session.
    wait_idle(&state, &id).await;
    assert_eq!(row(&state, &id).status_reason, None);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_steer_is_refused_with_the_states_own_code() {
    // `idle`: there is no turn to steer, which is the code HS-016 added to the family.
    let idle = TempDir::new("steer-refused-idle");
    let idle_state = with_stub(state_in(&idle, Config::default()), fast_stub());
    let idle_served = Served::start(http::router(idle_state.clone()), GRACE).await;
    let idle_id = created_id(&post_create(idle_served.addr, "steer-refused-idle").await);
    wait_idle(&idle_state, &idle_id).await;
    assert_envelope(
        &post_steer(
            idle_served.addr,
            &idle_id,
            &format!(r#"{{"guidance":"{GUIDANCE}"}}"#),
        )
        .await,
        409,
        SESSION_IDLE,
    );

    // `stopped`: the spec's own example (item 8: prompt, steer and abort on a stopped session).
    drive(&idle_state, &idle_id, Trigger::IdleStop).await;
    assert_eq!(row(&idle_state, &idle_id).status, Status::Stopped);
    assert_envelope(
        &post_steer(
            idle_served.addr,
            &idle_id,
            &format!(r#"{{"guidance":"{GUIDANCE}"}}"#),
        )
        .await,
        409,
        SESSION_STOPPED,
    );
    idle_served.finish().await.expect("a clean shutdown");

    // An id the table does not hold is a 404, not a state code.
    let unknown = TempDir::new("steer-refused-unknown");
    let unknown_state = state_in(&unknown, Config::default());
    let unknown_served = Served::start(http::router(unknown_state), GRACE).await;
    assert_envelope(
        &post_steer(
            unknown_served.addr,
            "0123456789abcdef",
            &format!(r#"{{"guidance":"{GUIDANCE}"}}"#),
        )
        .await,
        404,
        NOT_FOUND,
    );
    unknown_served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_steer_body_of_the_wrong_shape_is_bad_body_before_anything_else() {
    let dir = TempDir::new("steer-bad-body");
    let state = with_stub(state_in(&dir, Config::default()), paced(100));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "steer-bad-body").await);
    wait_idle(&state, &id).await;
    assert_eq!(post_prompt(served.addr, &id).await.status, 202);
    wait_running(&state, &id).await;

    for body in [
        r#"{"guidance":""}"#,
        r#"{"guidance":"   "}"#,
        r#"{"guidance":7}"#,
        r#"{"guidance":null}"#,
        r#"{}"#,
        r#"{"text":"a steer"}"#,
        r#"{"guidance":"one","text":"two"}"#,
        r#"[]"#,
        r#"not json"#,
    ] {
        let refused = post_steer(served.addr, &id, body).await;
        assert_envelope(&refused, 422, BAD_BODY);
    }
    // No refusal reached the agent: a body that was refused steered nothing.
    assert!(
        steers(&state).is_empty(),
        "a refused body steered the agent: {:?}",
        steers(&state)
    );
    served.finish().await.expect("a clean shutdown");
}

/// The steers the app's own backend recorded: every `Call::Steer`, in order.
fn steers(state: &AppState) -> Vec<Call> {
    let holosmith::backend::Backends::Stub(stub) = &state.backend else {
        return Vec::new();
    };
    stub.calls()
        .into_iter()
        .filter(|call| *call == Call::Steer)
        .collect()
}

#[test]
fn the_steer_route_is_recorded_mutating() {
    let app = http::App::new(
        http::router(state()),
        Guards::from_config(&Config::default()),
    );
    let steer = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}/steer")
        .expect("the steer is registered");
    assert_eq!(steer.method, Method::POST);
    assert_eq!(steer.kind, RouteKind::Mutating);
}
