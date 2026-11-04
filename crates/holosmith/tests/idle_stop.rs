//! The idle stop (HS-021): a session nobody is using stops costing money.
//!
//! The policy is a task rather than an endpoint, so the tests call its one pass — `Sweeper::sweep` is
//! public for exactly that reason, as `Deleter::remove` and `Ladder::end_turn` are — and assert the row,
//! the backend call and the event. `idle` is produced the way the product produces it (a fast stub
//! provisioning), and the states a sweep must never touch are driven through the one state function,
//! because no endpoint puts a session in `running`, `error` or `deleting` without a turn or a delete
//! behind it.
//!
//! Every test ages `last_active_at` rather than waiting, which is the same lever HS-017's review made a
//! rule: the column is whole seconds, so "it has been quiet for an hour" has to be a fact, not a race.

use std::net::SocketAddr;
use std::sync::Arc;

use holosmith::backend::Call;
use holosmith::config::Config;
use holosmith::http::{self, AppState};
use holosmith::sessions::Session;
use holosmith::sessions::Status;
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::sweeper::{IDLE_STOP, Sweeper};
use serde_json::Value;

mod support;
use support::{
    PATIENCE, Response, Served, TempDir, fast_stub, request, slow_stub, state_in, with_stub,
};

const GRACE: std::time::Duration = std::time::Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

/// A config whose idle stop fires after `minutes`, with a cap big enough for a test's own sessions.
fn idle_stop_after(minutes: u64) -> Config {
    Config {
        idle_stop_min: minutes,
        max_live_sessions: 8,
        ..Config::default()
    }
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

/// A create that must be accepted, answering its id.
fn created_id(response: &Response) -> String {
    assert_eq!(response.status, 202, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("a create answers JSON");
    body["id"]
        .as_str()
        .expect("the response names an id")
        .to_string()
}

fn row(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

fn live(state: &AppState) -> u64 {
    let database = state.database.lock().expect("the lock is not poisoned");
    database.sessions().live_count().expect("the count reads")
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

/// Push a session's `last_active_at` into the past, so "quiet for longer than the bound" is a fact
/// rather than a wait.
fn age(state: &AppState, id: &str, seconds: i64) {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .touch(id, holosmith::clock::unix_now() - seconds)
        .expect("the row is touched");
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not happen within {PATIENCE:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn wait_status(state: &AppState, id: &str, want: Status) {
    wait_until("the session reached the expected state", || {
        row(state, id).status == want
    })
    .await;
}

/// One state change through the one state function, for the states no endpoint produces on its own.
async fn drive(state: &AppState, id: &str, trigger: Trigger, reason: Option<&str>) {
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, reason)
        .await
        .expect("the move is in the table");
}

/// The sweeper this state's config asks for: the wiring `serve` uses, not a second one.
fn sweeper(state: &AppState) -> Sweeper {
    state.sweeper().expect("this config keeps the idle stop")
}

/// What the backend was asked to do for `id`, without the provisioning a create always does.
fn lifecycle(stub: &holosmith::backend::StubBackend, id: &str) -> Vec<Call> {
    stub.recorded()
        .into_iter()
        .filter(|call| call.session == id)
        .map(|call| call.call)
        .filter(|call| !matches!(call, Call::Provision(_) | Call::Undo(_)))
        .collect()
}

#[tokio::test]
async fn a_quiet_idle_session_is_stopped_and_its_slot_is_freed() {
    let dir = TempDir::new("idle-stop-round-trip");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, idle_stop_after(1)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "idle-stop-quiet").await);
    wait_status(&state, &id, Status::Idle).await;
    assert_eq!(live(&state), 1, "an idle session holds a slot");
    let worktree = row(&state, &id).worktree.clone();

    // An hour of quiet against a one-minute bound: the sweep's own comparison decides, not the test's.
    age(&state, &id, 3600);
    sweeper(&state).sweep().await;

    let stopped = row(&state, &id);
    assert_eq!(stopped.status, Status::Stopped, "the sweep did not stop it");
    assert_eq!(
        stopped.status_reason.as_deref(),
        Some(IDLE_STOP),
        "the row does not say why it stopped"
    );
    assert!(
        stopped.stopped_at.is_some(),
        "`stopped_at` moves with the state"
    );
    assert_eq!(
        stopped.worktree, worktree,
        "the idle stop changed the worktree path"
    );
    assert_eq!(live(&state), 0, "stopping did not free the slot");

    // The backend's stop is best effort in a task of its own, so it is waited for; the *row*, above,
    // is what the policy promised.
    wait_until("the stop reached the backend", || {
        lifecycle(&stub, &id).contains(&Call::Stop)
    })
    .await;
    assert_eq!(
        lifecycle(&stub, &id),
        [Call::Stop],
        "the idle stop did more to the backend than stop it"
    );

    // The story's fourth bullet: the transition is in the *event* log, with the reason a client reads.
    // (The stderr line per stop is the gate's to observe: nothing in `tests/` captures tracing output.)
    wait_until("the stop's event is committed", || {
        events(&state, &id)
            .iter()
            .any(|(_, kind, payload)| kind == "session.status" && payload["reason"] == IDLE_STOP)
    })
    .await;
    let (_, kind, payload) = events(&state, &id)
        .into_iter()
        .find(|(_, _, payload)| payload["reason"] == IDLE_STOP)
        .expect("the log carries the stop");
    assert_eq!(kind, "session.status");
    assert_eq!(payload["from"], "idle");
    assert_eq!(payload["to"], "stopped");
    assert_eq!(payload["step"], Value::Null);
}

#[tokio::test]
async fn a_session_inside_the_bound_is_left_alone() {
    let dir = TempDir::new("idle-stop-inside-bound");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, idle_stop_after(1)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "idle-stop-recent").await);
    wait_status(&state, &id, Status::Idle).await;
    // Thirty seconds of quiet against a one-minute bound: the sweep's comparison, not the sweep's
    // arithmetic, is what this asserts.
    age(&state, &id, 30);
    sweeper(&state).sweep().await;

    assert_eq!(
        row(&state, &id).status,
        Status::Idle,
        "the bound was ignored"
    );
    assert_eq!(row(&state, &id).status_reason, None);
    assert_eq!(live(&state), 1);
    assert!(
        !lifecycle(&stub, &id).contains(&Call::Stop),
        "a session inside its bound was stopped"
    );
}

#[tokio::test]
async fn every_quiet_session_is_stopped_by_one_sweep() {
    // "All of them, not one": a tick that stopped a single session per pass would make the idle stop's
    // latency a function of how many sessions the operator left alone.
    let dir = TempDir::new("idle-stop-all");
    let state = with_stub(state_in(&dir, idle_stop_after(1)), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let first = created_id(&post_create(served.addr, "idle-stop-all-1").await);
    let second = created_id(&post_create(served.addr, "idle-stop-all-2").await);
    let recent = created_id(&post_create(served.addr, "idle-stop-all-3").await);
    for id in [&first, &second, &recent] {
        wait_status(&state, id, Status::Idle).await;
    }
    age(&state, &first, 3600);
    age(&state, &second, 3600);
    age(&state, &recent, 30);

    sweeper(&state).sweep().await;

    for id in [&first, &second] {
        assert_eq!(
            row(&state, id).status,
            Status::Stopped,
            "{id} was quiet for an hour and survived the sweep"
        );
        assert_eq!(row(&state, id).status_reason.as_deref(), Some(IDLE_STOP));
    }
    assert_eq!(
        row(&state, &recent).status,
        Status::Idle,
        "the sweep stopped a session inside its bound"
    );
    assert_eq!(live(&state), 1);
}

#[tokio::test]
async fn a_running_error_or_deleting_session_is_not_a_candidate() {
    // The story's second bullet, from the side that matters: a sweep must not end a turn, must not
    // resurrect a failed session and must not interfere with a delete that is in flight.
    let dir = TempDir::new("idle-stop-other-states");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, idle_stop_after(1)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let running = created_id(&post_create(served.addr, "idle-stop-running").await);
    let failed = created_id(&post_create(served.addr, "idle-stop-failed").await);
    let deleting = created_id(&post_create(served.addr, "idle-stop-deleting").await);
    for id in [&running, &failed, &deleting] {
        wait_status(&state, id, Status::Idle).await;
    }
    drive(&state, &running, Trigger::Prompt, None).await;
    drive(&state, &failed, Trigger::UnexpectedExit, Some("boom")).await;
    drive(&state, &deleting, Trigger::Delete, None).await;

    for id in [&running, &failed, &deleting] {
        age(&state, id, 3600);
    }
    sweeper(&state).sweep().await;

    assert_eq!(row(&state, &running).status, Status::Running);
    assert_eq!(row(&state, &failed).status, Status::Error);
    assert_eq!(row(&state, &deleting).status, Status::Deleting);
    assert_eq!(
        row(&state, &running).status_reason,
        None,
        "the sweep wrote a reason on a running session"
    );
    assert_eq!(
        row(&state, &failed).status_reason.as_deref(),
        Some("boom"),
        "the sweep rewrote a failed session's reason"
    );
    for id in [&running, &failed, &deleting] {
        assert!(
            !lifecycle(&stub, id).contains(&Call::Stop),
            "{id} is not idle and was stopped anyway"
        );
    }
}

#[tokio::test]
async fn a_starting_session_is_not_a_candidate() {
    // A slow stub holds the row in `starting` while the test looks, which is the only way to have one.
    // A `starting` session is live and its provisioner is working: stopping it would leave resources
    // the operator never asked to lose.
    let dir = TempDir::new("idle-stop-starting");
    let stub = slow_stub();
    let state = with_stub(state_in(&dir, idle_stop_after(1)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "idle-stop-starting").await);
    assert_eq!(row(&state, &id).status, Status::Starting);
    age(&state, &id, 3600);
    sweeper(&state).sweep().await;

    assert_eq!(row(&state, &id).status, Status::Starting);
    assert!(!lifecycle(&stub, &id).contains(&Call::Stop));
}

#[tokio::test]
async fn the_bound_is_the_config_key_in_minutes() {
    // `sweeper()` is the one place the key is read, so its unit is proved here rather than in a demo.
    // Three points, not two: ninety seconds inside a two-minute bound stays, and one hundred and thirty
    // and three hundred both stop — so the bound is pinned to `(90s, 130s]` and a ×1.5 mutation of the
    // constant cannot pass. Two points would only have proved "minutes, not seconds".
    let dir = TempDir::new("idle-stop-bound-units");
    let state = with_stub(state_in(&dir, idle_stop_after(2)), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let inside = created_id(&post_create(served.addr, "idle-stop-units-inside").await);
    let just_past = created_id(&post_create(served.addr, "idle-stop-units-just-past").await);
    let well_past = created_id(&post_create(served.addr, "idle-stop-units-well-past").await);
    for id in [&inside, &just_past, &well_past] {
        wait_status(&state, id, Status::Idle).await;
    }
    age(&state, &inside, 90);
    age(&state, &just_past, 130);
    age(&state, &well_past, 300);

    sweeper(&state).sweep().await;

    assert_eq!(
        row(&state, &inside).status,
        Status::Idle,
        "90 seconds of quiet against a two-minute bound was stopped"
    );
    for id in [&just_past, &well_past] {
        assert_eq!(
            row(&state, id).status,
            Status::Stopped,
            "{id} was quiet for longer than two minutes and survived the sweep"
        );
    }
}

#[tokio::test]
async fn only_idle_rows_are_even_read_as_candidates() {
    // The sweep's other guard is the table (a `running + idle_stop` move is not a row), so a sweep over
    // an old `running` session is refused either way and no assertion on the row can tell which guard
    // fired. This reads the query itself, which is where the story's second bullet lives: an old
    // `running` session is not a candidate at all, so the sweep has nothing to ask the table about.
    let dir = TempDir::new("idle-stop-candidates");
    let state = with_stub(state_in(&dir, idle_stop_after(1)), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let idle = created_id(&post_create(served.addr, "idle-stop-candidates-idle").await);
    let busy = created_id(&post_create(served.addr, "idle-stop-candidates-busy").await);
    wait_status(&state, &idle, Status::Idle).await;
    wait_status(&state, &busy, Status::Idle).await;
    drive(&state, &busy, Trigger::Prompt, None).await;
    age(&state, &idle, 3600);
    age(&state, &busy, 3600);

    let cutoff = holosmith::clock::unix_now() - 60;
    let candidates = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .idle_before(cutoff)
            .expect("the candidates read")
    };
    let ids: Vec<&str> = candidates
        .iter()
        .map(|session| session.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [idle.as_str()],
        "the candidate read returned a session the sweep may not stop"
    );
}

#[tokio::test]
async fn zero_is_the_off_switch() {
    // ADR-0025: `idle_stop_min = 0` spawns no sweeper at all, so there is no task sweeping a database
    // every few seconds for a policy the operator turned off. The default still keeps one, which is
    // what makes the off switch a choice rather than an accident.
    let off = TempDir::new("idle-stop-off");
    let state = with_stub(state_in(&off, idle_stop_after(0)), fast_stub());
    assert!(state.sweeper().is_none(), "0 spawned a sweeper");

    let on = TempDir::new("idle-stop-default");
    let state = with_stub(state_in(&on, Config::default()), fast_stub());
    assert_eq!(state.config.idle_stop_min, 30, "the default moved");
    assert!(
        state.sweeper().is_some(),
        "the default turned the sweeper off"
    );
}
