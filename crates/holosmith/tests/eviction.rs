//! The cap and eviction: a create on a full machine stops the least recently active `idle`
//! session and takes its slot. Nothing is deleted — the victim keeps its history, its worktree path and
//! its events — and a machine with nothing idle refuses the create with `409 cap_reached` instead.
//!
//! Two states are driven rather than produced: `running` and `error` have no endpoint that puts a
//! session there (the prompt's turn runner and a container's death own them), so the tests that need
//! them call the one state function, as the forced rung and the stop do. `idle` is produced
//! the way the product produces it, by letting a fast stub provision.

use std::net::SocketAddr;
use std::sync::Arc;

use holosmith::backend::Call;
use holosmith::config::Config;
use holosmith::http::error::CAP_REACHED;
use holosmith::http::{self, AppState};
use holosmith::sessions::Session;
use holosmith::sessions::Status;
use holosmith::sessions::state::{Transitioner, Trigger};
use serde_json::Value;

mod support;
use support::{
    PATIENCE, Response, Served, TempDir, fast_stub, request, slow_stub, state_in, with_stub,
};

const GRACE: std::time::Duration = std::time::Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

/// A config holding `live` live sessions at once.
fn cap(live: u32) -> Config {
    Config {
        max_live_sessions: live,
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

/// The ADR-0008 envelope for `code`, all three parts asserted.
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

/// Push a session's `last_active_at` into the past, so "least recently active" is a fact rather than a
/// race between two touches in the same second.
fn age(state: &AppState, id: &str, seconds: i64) {
    age_at(state, id, holosmith::clock::unix_now() - seconds);
}

fn age_at(state: &AppState, id: &str, at: i64) {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .touch(id, at)
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

/// One state change through the one state function, for the states no endpoint produces.
async fn drive(state: &AppState, id: &str, trigger: Trigger, reason: Option<&str>) {
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, reason)
        .await
        .expect("the move is in the table");
}

/// The eviction and the removal are different things, so every assertion about "nothing was deleted"
/// reads the backend's calls: a `Stop` is the cap making room, a `Delete` would be data loss, and the
/// `Snapshot` before the `Stop` is keeping the victim's uncommitted work.
fn lifecycle(stub: &holosmith::backend::StubBackend, id: &str) -> Vec<Call> {
    stub.recorded()
        .into_iter()
        .filter(|call| call.session == id)
        .map(|call| call.call)
        .filter(|call| !matches!(call, Call::Provision(_) | Call::Undo(_)))
        .collect()
}

#[tokio::test]
async fn a_create_at_the_cap_stops_the_idle_session_and_takes_its_slot() {
    let dir = TempDir::new("evict-round-trip");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, cap(1)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let first = created_id(&post_create(served.addr, "evict-first").await);
    wait_status(&state, &first, Status::Idle).await;
    // The victim's log, as it stands before the eviction: its own provisioning and settle. The event
    // writer commits on its own tick, so the log is waited for rather than sampled.
    wait_until("the first session's events are committed", || {
        !events(&state, &first).is_empty()
    })
    .await;
    let before = events(&state, &first);
    let worktree = row(&state, &first).worktree.clone();

    let second = created_id(&post_create(served.addr, "evict-second").await);
    assert_ne!(second, first, "the create answered the session it replaced");

    // The eviction is the create's, so the victim is already `stopped` when the 202 arrives — no wait,
    // no poll: that is what "evicts … then proceeds" means.
    let victim = row(&state, &first);
    assert_eq!(victim.status, Status::Stopped);
    assert_eq!(
        victim.status_reason.as_deref(),
        Some("evicted"),
        "the row does not say why it stopped"
    );
    assert!(
        victim.stopped_at.is_some(),
        "`stopped_at` moves with the state"
    );
    assert_eq!(
        victim.worktree, worktree,
        "the eviction changed the worktree path"
    );
    assert_eq!(
        live(&state),
        1,
        "the eviction freed no slot, or the create took two"
    );

    // Nothing was deleted: the backend stopped the victim and touched nothing else, and the
    // transcript is the one it had plus the eviction's own transition. The stop runs in a task of its
    // own (an answer must not wait for container work), so it is waited for — the *row*, above, is
    // what the 202 already promised.
    wait_until("the eviction stopped the victim's backend", || {
        lifecycle(&stub, &first).contains(&Call::Stop)
    })
    .await;
    assert_eq!(lifecycle(&stub, &first), [Call::Snapshot, Call::Stop]);
    assert_eq!(lifecycle(&stub, &second), []);
    wait_until("the eviction's event is committed", || {
        events(&state, &first).len() > before.len()
    })
    .await;
    let after = events(&state, &first);
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "the eviction rewrote the transcript's prefix"
    );
    let (_, kind, payload) = after.last().expect("the log grew, so it has a last event");
    assert_eq!(kind, "session.status");
    assert_eq!(payload["from"], "idle");
    assert_eq!(payload["to"], "stopped");
    assert_eq!(payload["reason"], "evicted");

    // And the session is still an addressable row, `stopped` and resumable — the whole point of
    // "nothing is deleted by the cap".
    let answer = support::fetch(served.addr, &format!("/api/sessions/{first}")).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer.body.contains(r#""status":"stopped""#),
        "{}",
        answer.body
    );
}

#[tokio::test]
async fn the_victim_is_the_least_recently_active_idle_session() {
    let dir = TempDir::new("evict-lru");
    let state = with_stub(state_in(&dir, cap(2)), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let older = created_id(&post_create(served.addr, "evict-older").await);
    let newer = created_id(&post_create(served.addr, "evict-newer").await);
    wait_status(&state, &older, Status::Idle).await;
    wait_status(&state, &newer, Status::Idle).await;

    // The *later-created* row is pushed into the past, which rules out "the oldest row" by
    // construction: `created_at` says the other one, and only `last_active_at` answers this. The
    // `id ASC` half of the order needs two rows of the same age, and has its own test below.
    age(&state, &newer, 600);
    let _third = created_id(&post_create(served.addr, "evict-third").await);

    assert_eq!(row(&state, &newer).status, Status::Stopped);
    assert_eq!(
        row(&state, &newer).status_reason.as_deref(),
        Some("evicted")
    );
    assert_eq!(
        row(&state, &older).status,
        Status::Idle,
        "the create evicted a session that was more recently active"
    );
    assert_eq!(live(&state), 2, "the cap is two, so two sessions are live");
}

#[tokio::test]
async fn two_idle_sessions_of_the_same_age_are_ordered_by_id() {
    // `last_active_at` is whole seconds, so two rows touched in the same second tie — which is why the
    // candidate's order is `last_active_at ASC, id ASC` and not the time alone. Ids are random hex
    // (ADR-0011), so the test reads them and asserts against whichever sorts first rather than
    // assuming; what it proves is that the tie-break exists and is the id.
    let dir = TempDir::new("evict-tie");
    let state = with_stub(state_in(&dir, cap(2)), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let first = created_id(&post_create(served.addr, "evict-tie-1").await);
    let second = created_id(&post_create(served.addr, "evict-tie-2").await);
    wait_status(&state, &first, Status::Idle).await;
    wait_status(&state, &second, Status::Idle).await;

    // The same instant for both, an hour ago: not "who was touched last" either.
    let at = holosmith::clock::unix_now() - 3600;
    age_at(&state, &first, at);
    age_at(&state, &second, at);
    let (smallest, other) = if first < second {
        (first.clone(), second.clone())
    } else {
        (second.clone(), first.clone())
    };
    assert_ne!(smallest, other, "the two creates answered one id");

    let _third = created_id(&post_create(served.addr, "evict-tie-3").await);

    assert_eq!(
        row(&state, &smallest).status,
        Status::Stopped,
        "the tie-break is not `id ASC`: {smallest} should have gone and {other} should have stayed"
    );
    assert_eq!(row(&state, &other).status, Status::Idle);
}

#[tokio::test]
async fn a_machine_with_nothing_idle_refuses_the_create() {
    let dir = TempDir::new("evict-nothing-idle");
    let stub = slow_stub();
    let state = with_stub(state_in(&dir, cap(1)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    // A minute per step: the filler is `starting` for as long as the test needs, and a `starting`
    // session is live but not evictable.
    let filler = created_id(&post_create(served.addr, "evict-filler").await);
    assert_eq!(row(&state, &filler).status, Status::Starting);

    let refused = post_create(served.addr, "evict-fresh").await;
    assert_envelope(&refused, 409, CAP_REACHED);
    let body: Value = serde_json::from_str(&refused.body).expect("an envelope is JSON");
    assert!(
        body["message"]
            .as_str()
            .expect("the envelope has a sentence")
            .contains("idle"),
        "the refusal does not say why nothing could be evicted: {}",
        refused.body
    );
    assert_eq!(
        row(&state, &filler).status,
        Status::Starting,
        "the refusal evicted a session"
    );
    assert_eq!(live(&state), 1);
    assert_eq!(lifecycle(&stub, &filler), []);
}

#[tokio::test]
async fn a_running_session_is_not_evictable_either() {
    let dir = TempDir::new("evict-running");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, cap(1)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let running = created_id(&post_create(served.addr, "evict-running").await);
    wait_status(&state, &running, Status::Idle).await;
    // A turn nobody will settle: the row is the table's, and stopping it is not this story's to do.
    drive(&state, &running, Trigger::Prompt, None).await;

    assert_envelope(
        &post_create(served.addr, "evict-fresh").await,
        409,
        CAP_REACHED,
    );
    assert_eq!(
        row(&state, &running).status,
        Status::Running,
        "the refusal stopped a running turn"
    );
    assert_eq!(lifecycle(&stub, &running), [], "the backend was called");
}

#[tokio::test]
async fn a_failed_session_does_not_take_a_slot() {
    let dir = TempDir::new("evict-error");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, cap(1)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let failed = created_id(&post_create(served.addr, "evict-error").await);
    wait_status(&state, &failed, Status::Idle).await;
    drive(
        &state,
        &failed,
        Trigger::UnexpectedExit,
        Some("the container died"),
    )
    .await;

    // `error` is not live, so the create has a free slot and evicts nothing.
    let fresh = created_id(&post_create(served.addr, "evict-fresh").await);
    assert_ne!(fresh, failed);
    assert_eq!(
        row(&state, &failed).status,
        Status::Error,
        "the create touched a session that held no slot"
    );
    assert_eq!(lifecycle(&stub, &failed), []);
    assert_eq!(live(&state), 1);
}
