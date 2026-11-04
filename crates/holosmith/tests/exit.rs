//! Exit classification (HS-030, spec item 6): "I stopped it" is a different row from "it died".
//!
//! Two halves, and they are proved differently:
//!
//! - **The classifier is pure** — `classify`'s four cases are unit tests in `src/exit.rs`, where a
//!   container would only be in the way.
//! - **The flag, the record and the log need a database** — this file drives the three production stop
//!   paths (the sweeper's, the create's eviction and the ladder's forced rung) and the three outcomes
//!   (expected, clean, failed) through the server's own wiring.
//!
//! **No test here starts a container.** The exit a real container produces cannot be observed in this
//! build at all: the container runs detached (ADR-0036) until HS-031 attaches pi's stdio, and `--rm`
//! removes it a moment after it stops (ADR-0037). HS-031 is the caller of `Exits::record`; what is
//! proved here is what that call *means* — the flag, the two transitions and the session log.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use holosmith::backend::Call;
use holosmith::clock;
use holosmith::config::Config;
use holosmith::exit::{Exit, Exits, LOG_FILE, PI_EXIT, classify};
use holosmith::http::{self, AppState};
use holosmith::ladder::{FORCED_ABORT, Ladder};
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::sessions::{NewSession, Session, Status};
use holosmith::stop::Stopper;
use serde_json::Value;

mod support;
use support::{PATIENCE, Response, Served, TempDir, fast_stub, request, state_in, with_stub};

/// The drain deadline a test gives `serve`, and the ladder's grace when a test wants rung 2 at once.
const GRACE: Duration = Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

// ---- setup helpers ---------------------------------------------------------------------------

/// A session in `starting`, written through the repository: every test drives the states it needs from
/// here, so the setup is not itself the thing under test.
fn starting_session(state: &AppState, key: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .create(
            &NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: Some(key.to_string()),
                base_commit: None,
            },
            clock::unix_now(),
        )
        .expect("a session is created")
}

fn row(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

/// The expected-stop flag, taken the way the classifier takes it — which clears it, so every test calls
/// this **once** per question (the second call answering `false` is the story's "taken once").
fn flag(state: &AppState, id: &str) -> bool {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .take_expected_stop(id)
        .expect("the flag reads")
}

fn set_flag(state: &AppState, id: &str, expected: bool) {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .set_expected_stop(id, expected)
        .expect("the flag writes");
}

/// Every committed event of `session`, as `(kind, payload)`, in seq order.
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

/// One state change through the one state function, for the states no endpoint produces on its own.
async fn drive(state: &AppState, id: &str, trigger: Trigger, reason: Option<&str>) {
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, reason)
        .await
        .expect("the move is in the table");
}

/// A session in `idle`: `starting -> idle` is the table's `pi_ready` row, which is what the provisioner
/// applies when its four steps finished.
async fn idle_session(state: &AppState, key: &str) -> Session {
    let session = starting_session(state, key);
    drive(state, &session.id, Trigger::PiReady, None).await;
    row(state, &session.id)
}

/// The classifier this state's database and data root give, which is what HS-031 will build.
fn exits(state: &AppState, dir: &TempDir) -> Exits {
    Exits::new(
        Arc::clone(&state.database),
        state.events.clone(),
        dir.path(),
    )
}

fn log_path(dir: &TempDir, id: &str) -> std::path::PathBuf {
    dir.path().join("sessions").join(id).join(LOG_FILE)
}

// ---- the flag, at each of the three production stop sites -------------------------------------

#[tokio::test]
async fn the_idle_stops_flag_is_written_before_the_backend_is_asked_to_stop() {
    let dir = TempDir::new("exit-flag-idle");
    let stub = fast_stub();
    let state = with_stub(
        state_in(
            &dir,
            Config {
                idle_stop_min: 1,
                max_live_sessions: 8,
                ..Config::default()
            },
        ),
        stub.clone(),
    );
    let session = idle_session(&state, "exit-flag-idle").await;
    // Quiet for an hour against a one-minute bound: the sweep's own comparison decides, not the test's.
    {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .touch(&session.id, clock::unix_now() - 3600)
            .expect("the row is touched");
    }
    state
        .sweeper()
        .expect("this config keeps the idle stop")
        .sweep()
        .await;
    assert_eq!(row(&state, &session.id).status, Status::Stopped);

    // The stop is spawned, so it is waited for; the flag is what the stop recorded *before* it asked.
    wait_until("the stop reached the backend", || {
        stub.calls().contains(&Call::Stop)
    })
    .await;
    assert!(
        flag(&state, &session.id),
        "the idle stop did not record that it was the server's"
    );
}

#[tokio::test]
async fn the_evictions_flag_is_written_before_the_backend_is_asked_to_stop() {
    let dir = TempDir::new("exit-flag-evicted");
    let stub = fast_stub();
    let state = with_stub(
        state_in(
            &dir,
            Config {
                max_live_sessions: 1,
                ..Config::default()
            },
        ),
        stub.clone(),
    );
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // The cap is one, so the second create must evict the first — the production path, not a hand move.
    let victim = created_id(&post_create(served.addr, "exit-flag-evicted-a").await);
    wait_until("the first session is idle", || {
        row(&state, &victim).status == Status::Idle
    })
    .await;
    let fresh = created_id(&post_create(served.addr, "exit-flag-evicted-b").await);
    assert_ne!(fresh, victim, "the create answered the session it evicted");
    wait_until("the victim says why it stopped", || {
        row(&state, &victim).status == Status::Stopped
    })
    .await;

    wait_until("the eviction's stop reached the backend", || {
        stub.calls().contains(&Call::Stop)
    })
    .await;
    assert!(
        flag(&state, &victim),
        "the eviction did not record that it was the server's"
    );
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_forced_rungs_flag_is_written_before_the_backend_is_asked_to_stop() {
    let dir = TempDir::new("exit-flag-forced");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let session = idle_session(&state, "exit-flag-forced").await;
    drive(&state, &session.id, Trigger::Prompt, None).await;

    // A grace of zero is spent before rung 1 runs, so the ladder goes straight to rung 2 — the forced
    // stop the abort, the timeout and the budget all reach.
    let ladder = Ladder::new(
        state.backend.clone(),
        Arc::clone(&state.database),
        state.events.clone(),
        Duration::ZERO,
    );
    let running = row(&state, &session.id);
    ladder.end_turn(&running, FORCED_ABORT).await;

    assert_eq!(
        row(&state, &session.id).status,
        Status::Stopped,
        "rung 2 did not move the row"
    );
    assert!(
        flag(&state, &session.id),
        "the forced rung did not record that it was the server's"
    );
}

#[tokio::test]
async fn the_stopper_records_the_stop_and_the_flag_is_not_a_latch() {
    // The story's first bullet in its smallest form: a stop through the one routine leaves the flag set.
    // (That the write *precedes* the call is a property of the routine's two lines and is not observable
    // from outside it — ADR-0038 says so; the mutation that swaps them stays green, and the review of
    // this story is where that is recorded rather than papered over.)
    let dir = TempDir::new("exit-flag-order");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub);
    let session = idle_session(&state, "exit-flag-order").await;
    Stopper::new(state.backend.clone(), Arc::clone(&state.database))
        .stop(&session)
        .await
        .expect("the stub stops");
    assert!(flag(&state, &session.id), "the stop did not record itself");
    // And it is a flag, not a latch: the classifier consumes it.
    assert!(
        !flag(&state, &session.id),
        "the flag stayed set after it was read"
    );
}

#[tokio::test]
async fn a_new_container_starts_without_the_flag_the_last_one_left() {
    // A resumed session must not carry the last container's flag into the next container's exit, which
    // is why the provisioner's container step clears it (the backend may not write a row, ADR-0019).
    let dir = TempDir::new("exit-flag-cleared");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub);
    let session = starting_session(&state, "exit-flag-cleared");
    set_flag(&state, &session.id, true);

    state.provisioner().run(row(&state, &session.id)).await;

    assert_eq!(row(&state, &session.id).status, Status::Idle);
    assert!(
        !flag(&state, &session.id),
        "the new container inherited the last one's expected stop"
    );
}

// ---- the three outcomes ------------------------------------------------------------------------

#[tokio::test]
async fn a_clean_exit_without_the_flag_stops_the_session_with_pi_exit() {
    let dir = TempDir::new("exit-clean");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let session = idle_session(&state, "exit-clean").await;

    exits(&state, &dir).record(&session, 0, "").await;

    let stopped = row(&state, &session.id);
    assert_eq!(
        stopped.status,
        Status::Stopped,
        "a clean exit is not an error"
    );
    assert_eq!(
        stopped.status_reason.as_deref(),
        Some(PI_EXIT),
        "the row does not say the agent ended"
    );
    assert!(stopped.stopped_at.is_some(), "`stopped_at` moves with it");

    wait_until("the exit's transition is committed", || {
        events(&state, &session.id).iter().any(|(kind, payload)| {
            kind == "session.status"
                && payload["from"] == "idle"
                && payload["to"] == "stopped"
                && payload["reason"] == PI_EXIT
        })
    })
    .await;
}

#[tokio::test]
async fn an_unexpected_exit_is_an_error_with_the_last_lines_and_a_session_log() {
    let dir = TempDir::new("exit-failed");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let session = idle_session(&state, "exit-failed").await;

    let stderr = "booting pi\n\
         error: the provider refused the dummy key\n\
         error: no turn will run\n\
         stack: frame 3\n\
         stack: frame 2\n\
         stack: frame 1\n\
         stack: frame 0\n";
    exits(&state, &dir).record(&session, 3, stderr).await;

    let failed = row(&state, &session.id);
    assert_eq!(failed.status, Status::Error, "a non-zero exit is an error");
    assert_eq!(
        failed.status_reason.as_deref(),
        Some(
            "error: no turn will run\n\
             stack: frame 3\n\
             stack: frame 2\n\
             stack: frame 1\n\
             stack: frame 0"
        ),
        "the reason is the last five lines it printed, and only those"
    );

    // The whole of stderr is kept, which is the half a phone cannot show (spec item 15).
    let log =
        std::fs::read_to_string(log_path(&dir, &session.id)).expect("the session log was written");
    assert_eq!(log, stderr, "the log is not what the process printed");

    wait_until("the failure's transition is committed", || {
        events(&state, &session.id).iter().any(|(kind, payload)| {
            kind == "session.status" && payload["to"] == "error" && payload["from"] == "idle"
        })
    })
    .await;
}

#[tokio::test]
async fn an_expected_exit_leaves_the_row_the_stop_already_moved() {
    let dir = TempDir::new("exit-expected");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let session = idle_session(&state, "exit-expected").await;
    // The stop's caller moved the row first (ADR-0020's ordering), and the process then died.
    drive(&state, &session.id, Trigger::IdleStop, Some("idle_stop")).await;
    set_flag(&state, &session.id, true);
    let before = row(&state, &session.id);

    exits(&state, &dir)
        .record(&session, 143, "terminated")
        .await;

    let after = row(&state, &session.id);
    assert_eq!(after.status, Status::Stopped, "the row was not stopped");
    assert_eq!(
        after.status_reason.as_deref(),
        Some("idle_stop"),
        "the exit relabelled a stop the server had already explained"
    );
    assert_eq!(before.status, after.status);
    assert_eq!(after.stopped_at, before.stopped_at);
    // The lines are still kept: the log is the record of what the process printed, not of what failed.
    assert_eq!(
        std::fs::read_to_string(log_path(&dir, &session.id)).expect("the session log was written"),
        "terminated\n"
    );
}

#[tokio::test]
async fn the_flag_is_taken_once_so_the_next_exit_is_classified_on_its_own() {
    // A flag left set would excuse the *next* crash. This is the sequence that would go wrong: a stop
    // whose exit nobody classified, then a crash.
    let dir = TempDir::new("exit-taken-once");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let session = idle_session(&state, "exit-taken-once").await;
    set_flag(&state, &session.id, true);

    // The first exit is the stop's: nothing is written, and the flag is consumed.
    exits(&state, &dir)
        .record(&session, 137, "killed by the stop")
        .await;
    assert_eq!(row(&state, &session.id).status, Status::Idle);

    // The second is nobody's: it is an error, with its own reason.
    exits(&state, &dir).record(&session, 1, "boom").await;
    let failed = row(&state, &session.id);
    assert_eq!(failed.status, Status::Error);
    assert_eq!(failed.status_reason.as_deref(), Some("boom"));
}

#[tokio::test]
async fn the_classifier_is_the_one_exit_record_uses() {
    // The pure function is the decision; `record` must not have a second opinion. One case from each
    // arm, asserted against `classify` itself, so a change to the table in `record` fails here.
    assert_eq!(classify(true, 143, "terminated"), Exit::Expected);
    assert_eq!(classify(false, 0, ""), Exit::Clean);
    assert!(matches!(classify(false, 2, "boom"), Exit::Failed { .. }));
}

// ---- the production create path (the two HTTP helpers the eviction test needs) -----------------

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

/// The id a create answered.
fn created_id(response: &Response) -> String {
    assert_eq!(response.status, 202, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("a create answers JSON");
    body["id"]
        .as_str()
        .expect("the response names an id")
        .to_string()
}
