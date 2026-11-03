//! Resume (HS-018): `POST /api/sessions/{id}/resume` answers 202 with the row now `starting`, the
//! provisioner walks it back to `idle`, and nothing else about the session changes — the id, the events
//! and the worktree path are what a stop leaves alone.
//!
//! **The stop is the table's.** Nothing in the API produces `stopped` yet: the spec makes it reachable
//! from `idle` by `evict`, `idle_stop` and `drain` (HS-020, HS-021, HS-044) and from a killed `running`
//! turn by `abort_ladder` (HS-016, which resumes at once), and the API table lists no stop endpoint. So
//! a test drives the one state function, exactly as HS-016's forced rung is driven, and this story owns
//! the way back.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::Method;
use holosmith::backend::{Backends, Step, StubBackend};
use holosmith::config::Config;
use holosmith::http::error::{
    CAP_REACHED, NOT_FOUND, SESSION_IDLE, SESSION_RUNNING, SESSION_STARTING, SESSION_STOPPED,
};
use holosmith::http::{self, AppState, Guards, RouteKind};
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::sessions::{Session, Status};
use serde_json::Value;

mod support;
use support::{
    PATIENCE, Response, Served, TempDir, fast_stub, request, state, state_in, with_stub,
};

const GRACE: Duration = Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

const PROMPT: &str = r#"{"prompt":"read the app shell"}"#;

async fn post_create(addr: SocketAddr, key: &str) -> Response {
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nIdempotency-Key: {key}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    );
    request(addr, &text).await
}

/// One resume: a POST with no body, but with the two headers every mutating request carries.
async fn post_resume(addr: SocketAddr, id: &str) -> Response {
    let text = format!(
        "POST /api/sessions/{id}/resume HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n"
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

async fn post_with_body(addr: SocketAddr, path: &str, body: &str) -> Response {
    let text = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
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

/// Every committed event of `session` as `(seq, kind, payload)`, in seq order.
fn events(state: &AppState, session: &str) -> Vec<(i64, String, Value)> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session, 0, 200)
        .expect("the events read")
        .into_iter()
        .map(|event| {
            (
                event.seq,
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

async fn wait_status(state: &AppState, id: &str, want: Status) {
    wait_until("the session reached the expected state", || {
        row(state, id).status == want
    })
    .await;
}

/// Move a session with the one state function, the way the policies that stop a session will.
async fn drive(state: &AppState, id: &str, trigger: Trigger) {
    Transitioner::new(std::sync::Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, None)
        .await
        .expect("the move is in the table");
}

#[tokio::test]
async fn a_resume_answers_the_starting_row_and_provisions_back_to_idle() {
    let dir = TempDir::new("resume-round-trip");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "resume-round-trip").await);
    wait_status(&state, &id, Status::Idle).await;

    drive(&state, &id, Trigger::IdleStop).await;
    let stopped = row(&state, &id);
    assert_eq!(stopped.status, Status::Stopped);
    assert!(
        stopped.stopped_at.is_some(),
        "stopped_at moves with the state"
    );
    // The row can be ahead of its events (the writer commits on its own tick), and a prefix assertion
    // over an empty log would pass vacuously: wait for the provisioning's `starting -> idle` before
    // capturing, and require that something is actually there.
    wait_until("the provisioning's events are committed", || {
        events(&state, &id)
            .iter()
            .any(|(_, kind, payload)| kind == "session.status" && payload["to"] == "idle")
    })
    .await;
    let before = events(&state, &id);
    assert!(
        !before.is_empty(),
        "the log was empty before the stop, so nothing would prove what survived"
    );
    let worktree = stopped.worktree.clone();

    let accepted = post_resume(served.addr, &id).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    assert_eq!(
        accepted.header("content-type"),
        Some("application/json"),
        "{}",
        accepted.body
    );
    let body: Value = serde_json::from_str(&accepted.body).expect("the answer is JSON");
    // The row the 202 answers with is `starting`: the transition is the handler's, the provisioning is
    // the background task's, and the click never waits for a step.
    assert_eq!(body["status"], "starting", "{}", accepted.body);
    assert_eq!(body["id"], id.as_str());

    wait_status(&state, &id, Status::Idle).await;
    let resumed = row(&state, &id);
    assert_eq!(resumed.id, id, "the same session came back");
    assert_eq!(resumed.status_reason, None);
    assert_eq!(
        resumed.stopped_at, None,
        "leaving `stopped` clears stopped_at"
    );
    assert_eq!(resumed.worktree, worktree, "the worktree path is unchanged");

    // The history survived: every event the stop left is a prefix of the log now, so the transcript a
    // client replays is the whole session, not what came after the resume. The row reached `idle` above
    // and its events are one commit behind, so wait for the resume's own transition first.
    wait_until("the resume's events are committed", || {
        events(&state, &id).iter().any(|(_, kind, payload)| {
            kind == "session.status" && payload["from"] == "stopped" && payload["to"] == "starting"
        })
    })
    .await;
    let after = events(&state, &id);
    assert!(
        after.len() > before.len(),
        "the resume emitted no events: {} -> {}",
        before.len(),
        after.len()
    );
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "the events before the resume were not a prefix of the log after it"
    );
    // And the resume's own report is in the log: `stopped -> starting` then `starting -> idle`.
    let transitions: Vec<(String, String)> = after
        .iter()
        .filter(|(_, kind, _)| kind == "session.status")
        .map(|(_, _, payload)| {
            (
                payload["from"].as_str().unwrap_or_default().to_string(),
                payload["to"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert!(
        transitions.contains(&("stopped".to_string(), "starting".to_string())),
        "no `stopped -> starting` in the log: {transitions:?}"
    );
    assert!(
        transitions.contains(&("starting".to_string(), "idle".to_string())),
        "no `starting -> idle` in the log: {transitions:?}"
    );
    // The demo for the happy path that `serve` cannot reach yet (nothing in the API stops a session):
    // `cargo test --test resume a_resume_answers -- --nocapture` prints the round trip.
    println!("the resume, as the log records it (id {id}):");
    for (from, to) in &transitions {
        println!("  {from:>8} -> {to}");
    }
    println!(
        "  events before the stop: {}, after the resume: {} (the earlier ones are a prefix)",
        before.len(),
        after.len()
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_resume_is_accepted_from_error_too_because_the_table_has_two_rows_for_it() {
    let dir = TempDir::new("resume-from-error");
    // A stub that fails the network step: provisioning ends in `error` with a reason, which is the
    // second state the table resumes from.
    let state = with_stub(
        state_in(&dir, Config::default()),
        StubBackend::new(Duration::from_millis(1)).failing_at(Step::Network),
    );
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "resume-from-error").await);
    wait_status(&state, &id, Status::Error).await;
    assert!(
        row(&state, &id).status_reason.is_some(),
        "an error has a reason"
    );

    let accepted = post_resume(served.addr, &id).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let body: Value = serde_json::from_str(&accepted.body).expect("the answer is JSON");
    assert_eq!(body["status"], "starting", "{}", accepted.body);
    // This stub fails every time, so the session ends `error` again — which is honest: the resume ran
    // the provisioner, and the provisioner is what says the step still fails.
    wait_status(&state, &id, Status::Error).await;
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_resume_is_refused_with_the_states_own_code() {
    // `idle`: nothing to bring back.
    let idle = TempDir::new("resume-refused-idle");
    let idle_state = with_stub(state_in(&idle, Config::default()), fast_stub());
    let idle_served = Served::start(http::router(idle_state.clone()), GRACE).await;
    let idle_id = created_id(&post_create(idle_served.addr, "resume-refused-idle").await);
    wait_status(&idle_state, &idle_id, Status::Idle).await;
    assert_envelope(
        &post_resume(idle_served.addr, &idle_id).await,
        409,
        SESSION_IDLE,
    );

    // `running`: a turn is live, and a resume would provision over it.
    assert_eq!(post_prompt(idle_served.addr, &idle_id).await.status, 202);
    wait_status(&idle_state, &idle_id, Status::Running).await;
    assert_envelope(
        &post_resume(idle_served.addr, &idle_id).await,
        409,
        SESSION_RUNNING,
    );
    idle_served.finish().await.expect("a clean shutdown");

    // `starting`: the slow stub leaves a create provisioning for a minute.
    let starting = TempDir::new("resume-refused-starting");
    let starting_state = state_in(&starting, Config::default());
    let starting_served = Served::start(http::router(starting_state.clone()), GRACE).await;
    let starting_id =
        created_id(&post_create(starting_served.addr, "resume-refused-starting").await);
    assert_envelope(
        &post_resume(starting_served.addr, &starting_id).await,
        409,
        SESSION_STARTING,
    );
    starting_served.finish().await.expect("a clean shutdown");

    // An id the table does not hold is a 404, not a state code.
    let unknown = TempDir::new("resume-refused-unknown");
    let unknown_state = state_in(&unknown, Config::default());
    let unknown_served = Served::start(http::router(unknown_state), GRACE).await;
    assert_envelope(
        &post_resume(unknown_served.addr, "0123456789abcdef").await,
        404,
        NOT_FOUND,
    );
    unknown_served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_resume_at_the_cap_is_refused_and_leaves_the_session_stopped() {
    let dir = TempDir::new("resume-at-cap");
    let config = Config {
        max_live_sessions: 1,
        ..Config::default()
    };
    let state = with_stub(state_in(&dir, config), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // One session, stopped: a stopped session is not live, so it does not count against the cap.
    let stopped_id = created_id(&post_create(served.addr, "resume-at-cap-stopped").await);
    wait_status(&state, &stopped_id, Status::Idle).await;
    drive(&state, &stopped_id, Trigger::IdleStop).await;

    // A second session takes the only slot.
    let live_id = created_id(&post_create(served.addr, "resume-at-cap-live").await);
    assert_ne!(live_id, stopped_id);
    wait_status(&state, &live_id, Status::Idle).await;

    // Resuming the stopped one would make two live sessions, so the answer is the create's own code and
    // the row is not moved: a session refused at the cap must not be left `starting`.
    assert_envelope(
        &post_resume(served.addr, &stopped_id).await,
        409,
        CAP_REACHED,
    );
    assert_eq!(row(&state, &stopped_id).status, Status::Stopped);
    assert!(row(&state, &stopped_id).stopped_at.is_some());
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn prompt_steer_and_abort_on_a_stopped_session_are_session_stopped() {
    // The refusals the page's resume-and-retry hangs off (HS-018's third bullet, spec item 8).
    let dir = TempDir::new("resume-stopped-refusals");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "resume-stopped-refusals").await);
    wait_status(&state, &id, Status::Idle).await;
    drive(&state, &id, Trigger::IdleStop).await;

    assert_envelope(&post_prompt(served.addr, &id).await, 409, SESSION_STOPPED);
    assert_envelope(
        &post_with_body(
            served.addr,
            &format!("/api/sessions/{id}/steer"),
            r#"{"guidance":"too late"}"#,
        )
        .await,
        409,
        SESSION_STOPPED,
    );
    assert_envelope(
        &post_with_body(served.addr, &format!("/api/sessions/{id}/abort"), "").await,
        409,
        SESSION_STOPPED,
    );
    served.finish().await.expect("a clean shutdown");
}

#[test]
fn the_resume_route_is_recorded_mutating() {
    let app = http::App::new(
        http::router(state()),
        Guards::from_config(&Config::default()),
    );
    let resume = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}/resume")
        .expect("the resume is registered");
    assert_eq!(resume.method, Method::POST);
    assert_eq!(resume.kind, RouteKind::Mutating);
    // The three refusals this story leans on are one family, so a client matches the family.
    assert_eq!(SESSION_STOPPED, "session_stopped");
    assert_eq!(SESSION_IDLE, "session_idle");
    assert_eq!(SESSION_RUNNING, "session_running");
    assert_eq!(SESSION_STARTING, "session_starting");
    assert_eq!(CAP_REACHED, "cap_reached");
    // The stub's own handle is reachable through the state, so the test above can assert on it.
    assert!(matches!(state().backend, Backends::Stub(_)));
}
