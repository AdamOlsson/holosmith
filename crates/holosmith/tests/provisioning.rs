//! The provisioner, end to end: a create hands off to the background task, the four steps arrive
//! as `session.status` events, and the session ends `idle` — or `error` with a reason and no
//! partial resources.
//!
//! Two ways of watching are used on purpose. The event log is read from the database for the
//! rules that are about *what was recorded* — the step order, the payload shape, exactly one
//! provisioning per session — because it is the durable record and it needs no socket. The SSE
//! stream is read where the rule is about what a browser actually sees. Both start the real
//! `App` on an ephemeral port, so the create is the create a client makes.

use std::net::SocketAddr;
use std::time::Duration;

use holosmith::backend::{Backends, Call, Step, StubBackend};
use holosmith::config::Config;
use holosmith::http::{self, AppState};
use holosmith::sessions::state::SESSION_STATUS;
use holosmith::sessions::{Session, Status};
use serde_json::Value;

mod support;
use support::{PATIENCE, Response, Served, Sse, TempDir, request, state_in, with_stub};

/// The drain deadline a test gives `serve`: a socket it forgot to drop cannot hold the server for
/// the production ten seconds.
const GRACE: Duration = Duration::from_millis(100);

/// A stub that spends `millis` on each step: short enough that four steps finish inside a test,
/// long enough that the events are four separate commits.
fn paced(millis: u64) -> StubBackend {
    StubBackend::new(Duration::from_millis(millis))
}

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

const ORIGIN: &str = "http://localhost:8080";

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

fn created_id(response: &Response) -> String {
    assert_eq!(response.status, 202, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("a create answers JSON");
    body["id"]
        .as_str()
        .expect("the response names an id")
        .to_string()
}

/// Every `session.status` event of `session`, committed and in seq order.
fn status_events(state: &AppState, session: &str) -> Vec<(String, Value)> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session, 0, 100)
        .expect("the events read")
        .into_iter()
        .map(|event| {
            let payload: Value =
                serde_json::from_str(&event.payload).expect("a session.status payload is JSON");
            (event.kind, payload)
        })
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

fn row(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

async fn settled(state: &AppState, id: &str) {
    wait_until("the session left starting", || {
        row(state, id).status != Status::Starting
    })
    .await;
}

/// Wait until `count` events of `session` are committed, then read them all.
///
/// "Committed" is the point: the provisioner's status write happens *before* the event, and the
/// writer commits a batch every 25 ms, so a row that already says `idle` can be a few milliseconds
/// ahead of the event that reports it. Reading the log without this wait would be a race, and a
/// flaky one.
async fn committed_status_events(state: &AppState, id: &str, count: usize) -> Vec<(String, Value)> {
    wait_until("the session.status events commit", || {
        status_events(state, id).len() >= count
    })
    .await;
    status_events(state, id)
}

/// The step words of the events, in order, with the terminal event's `null` as `"-"`.
fn steps(events: &[(String, Value)]) -> Vec<String> {
    events
        .iter()
        .map(|(_, payload)| match payload["step"].as_str() {
            Some(step) => step.to_string(),
            None => "-".to_string(),
        })
        .collect()
}

// ---- the four steps, then idle -------------------------------------------------------

#[tokio::test]
async fn a_create_provisions_the_four_steps_in_order_and_ends_idle() {
    let dir = TempDir::new("provisioning-steps");
    let state = with_stub(state_in(&dir, Config::default()), paced(1));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "steps").await);

    settled(&state, &id).await;
    let events = committed_status_events(&state, &id, 5).await;
    assert!(
        events.iter().all(|(kind, _)| kind == SESSION_STATUS),
        "a provisioning event carried another kind: {events:?}"
    );
    assert_eq!(
        steps(&events),
        ["worktree", "network", "container", "pi", "-"],
        "the steps did not arrive in the spec's order: {events:?}"
    );
    // The four step events report no state change; the last reports the transition.
    let reported: Vec<Option<&str>> = events
        .iter()
        .map(|(_, payload)| payload["to"].as_str())
        .collect();
    assert_eq!(
        reported,
        [
            Some("starting"),
            Some("starting"),
            Some("starting"),
            Some("starting"),
            Some("idle"),
        ]
    );
    assert!(
        events
            .iter()
            .all(|(_, payload)| payload["reason"].is_null()),
        "a successful provisioning carried a reason: {events:?}"
    );
    // The success ending the story names: `idle`, not `starting`.
    let settled_row = row(&state, &id);
    assert_eq!(settled_row.status, Status::Idle);
    assert_eq!(settled_row.status_reason, None);
    assert_eq!(
        settled_row.worktree, "",
        "the stub creates no worktree, so the row keeps ''"
    );

    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_scripted_agent_exit_reaches_the_row_through_the_server() {
    // after a successful pi step the provisioner hands the backend's exit watcher to a task of its
    // own, and that task records what the exit means (`Exits::record`). The stub's scripted exit is the only
    // way to prove that wiring without Docker — and the row's ending is the classifier's, not this test's:
    // a process that dies without the expected-stop flag is `error` with the tail of what it printed
    // (spec item 6).
    let dir = TempDir::new("provisioning-exit");
    let state = with_stub(
        state_in(&dir, Config::default()),
        paced(1).exiting(137, "the agent fell over"),
    );
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "exit").await);

    wait_until("the agent's exit is recorded", || {
        row(&state, &id).status == Status::Error
    })
    .await;
    let crashed = row(&state, &id);
    assert_eq!(crashed.status, Status::Error);
    assert_eq!(
        crashed.status_reason.as_deref(),
        Some("the agent fell over"),
        "the row's reason is not what the process printed"
    );

    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_steps_reach_a_browser_over_the_event_stream() {
    // The same five events, watched the way the phone watches them: the SSE `id` is the seq, the
    // `data` is the payload, and the last frame says the session is idle.
    let dir = TempDir::new("provisioning-stream");
    let state = with_stub(state_in(&dir, Config::default()), paced(1));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "stream").await);

    // A client that connects after provisioning finished still sees everything: with no
    // `Last-Event-ID` the stream replays the log from its start when the log fits the window
    // (ADR-0014, ADR-0033), and this session has four provisioning steps and nothing else.
    settled(&state, &id).await;
    let mut stream = Sse::connect(served.addr, &id, None).await;
    assert_eq!(stream.status, 200);
    assert_eq!(stream.header("content-type"), Some("text/event-stream"));

    let frames = stream.take(5, PATIENCE).await;
    let seqs: Vec<i64> = frames.iter().map(support::Frame::seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5], "the stream's ids are not the seqs");
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.kind.as_deref())
            .collect::<Vec<_>>(),
        [Some(SESSION_STATUS); 5]
    );
    assert_eq!(
        frames[0].data, r#"{"from":"starting","to":"starting","step":"worktree","reason":null}"#,
        "the first frame is not the worktree step"
    );
    assert_eq!(
        frames[4].data, r#"{"from":"starting","to":"idle","step":null,"reason":null}"#,
        "the last frame does not end the session idle"
    );
    assert_eq!(row(&state, &id).status, Status::Idle);

    drop(stream);
    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_answer_comes_back_before_the_first_step_finishes() {
    // The handler hands off and returns: with a stub that takes a minute per step the 202 arrives
    // while the session is still `starting`, which is spec item 7's whole point.
    let dir = TempDir::new("provisioning-handoff");
    let state = state_in(&dir, Config::default());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, "handoff").await;
    let id = created_id(&response);
    assert!(
        response.body.contains(r#""status":"starting""#),
        "the answer did not carry the starting row: {}",
        response.body
    );
    assert_eq!(row(&state, &id).status, Status::Starting);

    // The step it announced is on its way: the worktree event is committed before the answer's
    // row was read, without the answer having waited for the step itself.
    wait_until("the worktree step was announced", || {
        !status_events(&state, &id).is_empty()
    })
    .await;
    let events = committed_status_events(&state, &id, 1).await;
    assert_eq!(steps(&events), ["worktree"], "{events:?}");
    assert_eq!(
        row(&state, &id).status,
        Status::Starting,
        "the handler waited for a step instead of handing off"
    );

    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_retry_of_the_same_key_provisions_once() {
    // The create's idempotency covers provisioning too: a retry is answered from the row it
    // already has, so one session gets one set of steps (a second worker would emit five more
    // events and race the first).
    let dir = TempDir::new("provisioning-idempotent");
    let stub = paced(1);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "retry").await);
    settled(&state, &id).await;

    let retry = post_create(served.addr, "retry").await;
    assert_eq!(created_id(&retry), id, "the retry made a second session");
    // Give a second provisioner every chance to show itself before asserting it did not.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        steps(&committed_status_events(&state, &id, 5).await),
        ["worktree", "network", "container", "pi", "-"],
        "the retry provisioned the session a second time"
    );
    assert_eq!(
        stub.calls()
            .iter()
            .filter(|call| matches!(call, Call::Provision(_)))
            .count(),
        4,
        "the stub was asked for more than one provisioning"
    );

    served.finish().await.expect("a clean shutdown");
}

// ---- failure -----------------------------------------------------------------------

#[tokio::test]
async fn a_failure_at_a_named_step_undoes_what_completed_and_ends_error() {
    // The story's sixth acceptance, exactly: the stub is told to fail at `container`, so
    // `worktree` and `network` completed and are undone — and they are undone in the reverse of
    // the order they ran, which is the only order that can work for nested resources.
    let dir = TempDir::new("provisioning-failure");
    let stub = paced(1).failing_at(Step::Container);
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "failure").await);

    settled(&state, &id).await;
    let reason = "the stub backend was told to fail at the container step";
    let events = committed_status_events(&state, &id, 4).await;
    // Three steps were announced, then the transition; the failing step is the one the last
    // announcement named, because a step is announced before it runs and a transition
    // carries no step of its own.
    assert_eq!(
        steps(&events),
        ["worktree", "network", "container", "-"],
        "the failing step's event is missing or out of order: {events:?}"
    );
    let (_, last) = events.last().expect("the failure event exists");
    assert_eq!(last["from"].as_str(), Some("starting"));
    assert_eq!(last["to"].as_str(), Some("error"));
    assert_eq!(last["reason"].as_str(), Some(reason));
    assert!(
        last["step"].is_null(),
        "a transition carried a step: {last}"
    );
    assert_eq!(steps(&events)[2], "container", "the last step announced");

    // The row agrees with the event, and the reason is the same sentence a client was told.
    let failed = row(&state, &id);
    assert_eq!(failed.status, Status::Error);
    assert_eq!(failed.status_reason.as_deref(), Some(reason));

    // What was asked of the backend: the completed steps, undone most recent first, and nothing
    // undone for the step that failed or the one that never ran.
    let calls = stub.calls();
    let undone: Vec<Step> = calls
        .iter()
        .filter_map(|call| match call {
            Call::Undo(step) => Some(*step),
            _ => None,
        })
        .collect();
    assert_eq!(undone, [Step::Network, Step::Worktree], "{calls:?}");

    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn the_docker_backend_refuses_a_project_that_is_not_there() {
    // Without there is no Docker backend, and `--backend` defaults to it. asked a create
    // against a projects root holding no repository to end `error` with git's sentence; moved that
    // question to the create, so the answer is now the create's own 422 and no row at all — a session that
    // never existed cannot be resumed, and the operator reads the fix before anything is written. What a
    // Docker create of a *safe* project then does (the worktree step, then the network step's refusal) is
    // `tests/worktree.rs`'s end-to-end case.
    //
    // The root is this test's own empty directory rather than the config's default: a test that ran
    // against `/srv/holosmith/projects` would pass or fail depending on what the host happens to keep
    // there.
    let dir = TempDir::new("provisioning-docker");
    let projects = dir.file("projects");
    std::fs::create_dir_all(&projects).expect("the projects root is creatable");
    let config = Config {
        projects_root: projects.clone(),
        ..Config::default()
    };
    let mut state = state_in(&dir, config);
    state.backend = Backends::for_run(
        holosmith::runtime::Backend::Docker,
        holosmith::container::ContainerSettings::at(projects, dir.path()),
    );
    assert_eq!(
        state.backend.as_str(),
        "docker",
        "the run's backend did not reach the state"
    );
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let response = post_create(served.addr, "docker").await;

    assert_eq!(response.status, 422, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("an envelope is JSON");
    assert_eq!(
        body["code"], "git_not_directory",
        "the refusal does not name the hole: {}",
        response.body
    );
    let rows = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .list(None)
            .expect("the list reads")
            .len()
    };
    assert_eq!(rows, 0, "a refused create wrote a row");

    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn provisioning_leaves_nothing_on_disk() {
    // "Partial resources are not left behind" has a cheap, checkable form for the stub: the data
    // root holds the database and its WAL sidecars and nothing else, before and after.
    let dir = TempDir::new("provisioning-disk");
    let state = with_stub(state_in(&dir, Config::default()), paced(1));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "disk").await);
    settled(&state, &id).await;
    assert_eq!(row(&state, &id).status, Status::Idle);

    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("the data root is readable")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    for name in &names {
        assert!(
            name == "app.db" || name.starts_with("app.db-"),
            "provisioning wrote {name} into the data root: {names:?}"
        );
    }
    assert!(
        names.iter().any(|name| name == "app.db"),
        "the database is missing: {names:?}"
    );

    served.finish().await.expect("a clean shutdown");
}
