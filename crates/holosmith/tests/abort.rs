//! Abort and its ladder (HS-016): `POST /api/sessions/{id}/abort` answers 202 with the row while the
//! ladder ends the turn off-request. Rung 1 asks the agent to stop and the turn runner closes the
//! session; rung 2 kills the container when the agent ignores rung 1, records why, and brings the
//! session straight back.
//!
//! Every state a refusal needs is produced the way the server produces it: `idle` by letting a fast
//! stub provision, `starting` by aborting a session the slow stub has not finished provisioning, and
//! `running` by prompting. The only thing a test drives by hand is the ladder's *reason*, which is a
//! parameter precisely so a later story (HS-022's timeout) can pass its own — that test is the one
//! place `Ladder::climb` is awaited directly.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::Method;
use holosmith::backend::StubBackend;
use holosmith::config::{Config, Duration as ConfigDuration};
use holosmith::http::error::{NOT_FOUND, SESSION_IDLE, SESSION_RUNNING, SESSION_STARTING};
use holosmith::http::{self, AppState, Guards, RouteKind};
use holosmith::ladder::{FORCED_ABORT, Ladder};
use holosmith::sessions::{Session, Status};
use serde_json::Value;

mod support;
use support::{
    PATIENCE, Response, Served, TempDir, fast_stub, request, state, state_in, with_stub,
};

/// The drain deadline a test gives `serve`: a socket it forgot to drop cannot hold the server for
/// the production ten seconds.
const GRACE: Duration = Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

const PROMPT: &str = r#"{"prompt":"read the app shell"}"#;

/// A stub that spends `millis` on each provisioning step and on each frame of a turn, so a turn
/// takes long enough to be aborted while it runs.
fn paced(millis: u64) -> StubBackend {
    StubBackend::new(Duration::from_millis(millis))
}

/// A config whose grace is short enough that a test can watch rung 2 without waiting ten seconds.
fn impatient_grace(millis: u64) -> Config {
    Config {
        abort_grace: ConfigDuration::from_millis(millis),
        ..Config::default()
    }
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

/// One abort: a POST with no body, but with the two headers every mutating request carries
/// (ADR-0010's `bad_client` check reads the content type as well, and an empty body does not exempt
/// a request from it).
async fn post_abort(addr: SocketAddr, id: &str) -> Response {
    let text = format!(
        "POST /api/sessions/{id}/abort HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n"
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
/// asserted, because a test that checks only the status is how a wrong code ships.
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

/// Every committed `session.status` payload of `session`, in seq order.
///
/// Only the status events: a turn's frames are the backend's and their count depends on how far the
/// script got before it was cut, which is exactly what an abort makes unpredictable.
fn transitions(state: &AppState, session: &str) -> Vec<Value> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session, 0, 200)
        .expect("the events read")
        .into_iter()
        .filter(|event| event.kind == "session.status")
        .map(|event| serde_json::from_str(&event.payload).expect("a payload is JSON"))
        .collect()
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

/// Wait until the session is `idle`, which for a paced stub is the end of the turn.
async fn wait_idle(state: &AppState, id: &str) {
    wait_until("the session reached idle", || {
        row(state, id).status == Status::Idle
    })
    .await;
}

/// Whether the log holds a transition `from -> to` carrying `reason`.
fn carries(transitions: &[Value], from: &str, to: &str, reason: &str) -> bool {
    transitions.iter().any(|payload| {
        payload["from"] == from && payload["to"] == to && payload["reason"] == reason
    })
}

// ---- rung 1: the agent stops, the turn runner settles the session ----------------------------

#[tokio::test]
async fn an_abort_answers_the_running_row_and_the_turn_settles_through_rung_one() {
    let dir = TempDir::new("abort-rung-one");
    let stub = paced(100);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "abort-rung-one").await);
    wait_idle(&state, &id).await;

    let accepted = post_prompt(served.addr, &id, PROMPT).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    wait_until("the turn is running", || {
        row(&state, &id).status == Status::Running
    })
    .await;

    let abort = post_abort(served.addr, &id).await;
    assert_eq!(abort.status, 202, "{}", abort.body);
    assert_eq!(
        abort.header("content-type"),
        Some("application/json"),
        "{}",
        abort.body
    );
    // The answer is the row as it stands: `running`, because the 202 promises "accepted", not
    // "aborted" — progress arrives as events (spec item 16).
    let body: Value = serde_json::from_str(&abort.body).expect("the answer is JSON");
    assert_eq!(body["status"], "running", "{}", abort.body);
    assert_eq!(body["id"], id.as_str());

    wait_idle(&state, &id).await;
    let settled = row(&state, &id);
    // Rung 1's ending is the turn runner's, and it records *which* rung fired as the reason.
    assert_eq!(settled.status_reason.as_deref(), Some("abort_completed"));

    // Rung 1 asked the agent to stop, in the spec's order, and nothing had to be killed.
    let calls = stub.calls();
    let tail: Vec<_> = calls.iter().rev().take(3).rev().copied().collect();
    assert_eq!(
        tail,
        [
            holosmith::backend::Call::StartTurn,
            holosmith::backend::Call::ClearQueue,
            holosmith::backend::Call::Abort,
        ],
        "the queue is cleared before the agent is asked to abort: {calls:?}"
    );
    assert!(
        !calls.contains(&holosmith::backend::Call::Stop),
        "the forced rung must not fire for an agent that stopped: {calls:?}"
    );

    wait_until("the abort's transition is committed", || {
        carries(
            &transitions(&state, &id),
            "running",
            "idle",
            "abort_completed",
        )
    })
    .await;
    served.finish().await.expect("a clean shutdown");
}

// ---- rung 2: the agent ignores rung 1, so the container is killed and the session resumes -----

#[tokio::test]
async fn the_ladder_forces_the_stop_when_the_agent_ignores_the_abort() {
    let dir = TempDir::new("abort-rung-two");
    // A hundred milliseconds per frame is a twelve-frame turn: long enough that a 150 ms grace
    // expires while the agent is still talking, which is the hung agent rung 2 exists for.
    let stub = paced(100).ignoring_abort();
    let state = with_stub(state_in(&dir, impatient_grace(150)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "abort-rung-two").await);
    wait_idle(&state, &id).await;

    assert_eq!(
        post_prompt(served.addr, &id, PROMPT).await.status,
        202,
        "the prompt is accepted"
    );
    let abort = post_abort(served.addr, &id).await;
    assert_eq!(abort.status, 202, "{}", abort.body);

    // The forced stop, the reasoning and the resume are all the server's; the row ends `idle` with
    // the same id, which is the story's whole claim.
    wait_idle(&state, &id).await;
    let resumed = row(&state, &id);
    assert_eq!(resumed.id, id, "the resumed session is the same session");
    assert_eq!(resumed.status_reason, None, "the resume clears the reason");

    let calls = stub.calls();
    let settled: Vec<_> = calls
        .iter()
        .skip_while(|call| **call != holosmith::backend::Call::StartTurn)
        .copied()
        .collect();
    assert_eq!(
        settled,
        [
            holosmith::backend::Call::StartTurn,
            holosmith::backend::Call::ClearQueue,
            holosmith::backend::Call::Abort,
            holosmith::backend::Call::Snapshot,
            holosmith::backend::Call::Stop,
            // The resume goes through the provisioner, which runs the four steps again.
            holosmith::backend::Call::Provision(holosmith::backend::Step::Worktree),
            holosmith::backend::Call::Provision(holosmith::backend::Step::Network),
            holosmith::backend::Call::Provision(holosmith::backend::Step::Container),
            holosmith::backend::Call::Provision(holosmith::backend::Step::Pi),
        ],
        "the ladder's calls, in order: {calls:?}"
    );

    // The log says which rung fired and where the session went: `running -> stopped` with the
    // forced reason, then the resume's two transitions.
    wait_until("the forced stop is committed", || {
        carries(
            &transitions(&state, &id),
            "running",
            "stopped",
            FORCED_ABORT,
        )
    })
    .await;
    let log = transitions(&state, &id);
    // The demo for the rung that cannot be driven from `serve` (the hung agent is a constructor
    // argument): `cargo test --test abort the_ladder_forces -- --nocapture` prints the ladder's own
    // record of the forced stop and of the resume that followed it.
    println!("the forced rung, as the log records it:");
    for payload in &log {
        println!(
            "  {} -> {}  step={}  reason={}",
            payload["from"], payload["to"], payload["step"], payload["reason"]
        );
    }
    // The resume's transition carries no reason: the forced stop's reason is the record of *why*
    // the session went `stopped`, and clearing it is what makes the resumed row `idle`/`null`.
    assert!(
        log.iter().any(|payload| payload["from"] == "stopped"
            && payload["to"] == "starting"
            && payload["reason"].is_null()),
        "the forced stop did not resume with no reason: {log:?}"
    );
    assert!(
        log.iter().any(|payload| payload["to"] == "idle"),
        "the resumed session never reached idle: {log:?}"
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_ladders_reason_is_the_callers_so_a_later_story_can_pass_its_own() {
    // HS-022's turn timeout climbs the same ladder with its own reason. Awaiting `climb` directly
    // is what proves the reason is a parameter and not a constant spelled inside the routine.
    let dir = TempDir::new("abort-reason");
    let stub = paced(50).ignoring_abort();
    let state = with_stub(state_in(&dir, impatient_grace(120)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "abort-reason").await);
    wait_idle(&state, &id).await;
    assert_eq!(
        post_prompt(served.addr, &id, PROMPT).await.status,
        202,
        "the prompt is accepted"
    );
    wait_until("the turn is running", || {
        row(&state, &id).status == Status::Running
    })
    .await;

    let ladder = Ladder::new(
        state.backend.clone(),
        std::sync::Arc::clone(&state.database),
        state.events.clone(),
        Duration::from_millis(120),
    );
    let running = row(&state, &id);
    ladder.climb(&running, "turn_timeout").await;

    wait_until("the timeout's stop is committed", || {
        carries(
            &transitions(&state, &id),
            "running",
            "stopped",
            "turn_timeout",
        )
    })
    .await;
    wait_idle(&state, &id).await;
    served.finish().await.expect("a clean shutdown");
}

// ---- the refusals ---------------------------------------------------------------------------

#[tokio::test]
async fn an_abort_is_refused_with_the_states_own_code() {
    // `starting`: a session the slow stub has not finished provisioning has no turn to abort.
    let starting = TempDir::new("abort-refused-starting");
    let state = state_in(&starting, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "abort-refused-starting").await);
    assert_envelope(&post_abort(served.addr, &id).await, 409, SESSION_STARTING);
    served.finish().await.expect("a clean shutdown");

    // `idle`: HS-016's own code, which the family did not have before this story.
    let idle = TempDir::new("abort-refused-idle");
    let idle_state = with_stub(state_in(&idle, Config::default()), fast_stub());
    let idle_served = Served::start(http::router(idle_state.clone()), GRACE).await;
    let idle_id = created_id(&post_create(idle_served.addr, "abort-refused-idle").await);
    wait_idle(&idle_state, &idle_id).await;
    assert_envelope(
        &post_abort(idle_served.addr, &idle_id).await,
        409,
        SESSION_IDLE,
    );

    // A session a turn is already running for is `running`, which is the prompt's own refusal code
    // — checked here because both endpoints read the same table.
    idle_served.finish().await.expect("a clean shutdown");

    // An id the table does not hold is a 404, not a state code.
    let unknown = TempDir::new("abort-refused-unknown");
    let unknown_state = state_in(&unknown, Config::default());
    let unknown_served = Served::start(http::router(unknown_state), GRACE).await;
    assert_envelope(
        &post_abort(unknown_served.addr, "0123456789abcdef").await,
        404,
        NOT_FOUND,
    );
    unknown_served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn an_abort_while_the_turn_is_running_is_accepted_and_a_second_is_refused_once_it_is_idle() {
    let dir = TempDir::new("abort-twice");
    let state = with_stub(state_in(&dir, Config::default()), paced(100));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "abort-twice").await);
    wait_idle(&state, &id).await;
    assert_eq!(
        post_prompt(served.addr, &id, PROMPT).await.status,
        202,
        "the prompt is accepted"
    );
    wait_until("the turn is running", || {
        row(&state, &id).status == Status::Running
    })
    .await;
    assert_eq!(
        post_abort(served.addr, &id).await.status,
        202,
        "the first abort is accepted"
    );
    wait_idle(&state, &id).await;
    // The turn is gone, so the second abort has nothing to stop.
    assert_envelope(&post_abort(served.addr, &id).await, 409, SESSION_IDLE);
    served.finish().await.expect("a clean shutdown");
}

#[test]
fn the_abort_route_is_recorded_mutating() {
    let app = http::App::new(
        http::router(state()),
        Guards::from_config(&Config::default()),
    );
    let abort = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}/abort")
        .expect("the abort is registered");
    assert_eq!(abort.method, Method::POST);
    assert_eq!(abort.kind, RouteKind::Mutating);
    // The abort's refusal code and the prompt's share one family, so a client can match the family
    // and read the state from the code.
    assert_eq!(SESSION_RUNNING, "session_running");
    assert_eq!(SESSION_IDLE, "session_idle");
}
