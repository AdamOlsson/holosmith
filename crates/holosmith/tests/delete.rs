//! Delete (HS-019): `DELETE /api/sessions/{id}` answers 202 with the row now `deleting`, and the
//! removal — the backend's resources, the session's key and its row — happens off the request's
//! task. The snapshot is the request's, because a failed one must answer `409 unsaved_work` and must
//! leave the row exactly as it was; `?force=true` is the caller saying the work may go anyway.
//!
//! Two states a delete meets are produced the way the server produces them — `idle` by letting a fast
//! stub provision, `starting` by a create — and two are driven by hand: `running` (nothing in the API
//! stops or settles a turn the test wants held open) and `deleting` (the stub's removal is instant,
//! so the only way to see a row that a *failed* removal left behind is to put it there). Both use the
//! table's own function, as HS-016's forced rung and HS-018's stop do.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::http::Method;
use holosmith::backend::{Backends, Call, Provisioned, SessionBackend, Step};
use holosmith::config::{Config, Duration as ConfigDuration};
use holosmith::container::ContainerSettings;
use holosmith::deleter::DELETED;
use holosmith::http::error::{BAD_CLIENT, NOT_FOUND, UNSAVED_WORK};
use holosmith::http::{self, AppState, Guards, RouteKind};
use holosmith::ladder::Ending;
use holosmith::provisioner::Provisioner;
use holosmith::runtime::Backend;
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::sessions::{NewSession, Session, Status};
use serde_json::Value;

mod support;
use support::{
    PATIENCE, Response, Served, TempDir, fast_stub, request, state, state_in, with_stub,
};

/// The drain deadline a test gives `serve`, so a socket it forgot to drop cannot hold the server.
const GRACE: Duration = Duration::from_millis(100);

const ORIGIN: &str = "http://localhost:8080";

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

/// A config whose grace is short enough that a test can watch the ladder's rung 2 without waiting ten
/// seconds for a turn that will never settle.
fn impatient_grace(millis: u64) -> Config {
    Config {
        abort_grace: ConfigDuration::from_millis(millis),
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

/// One delete, with the query string the caller wants (`""`, `"?force=true"`, …) and the two headers
/// every mutating request carries — `bad_client` reads the content type as well, and a bodyless
/// request is not exempt.
async fn delete(addr: SocketAddr, id: &str, query: &str) -> Response {
    let text = format!(
        "DELETE /api/sessions/{id}{query} HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n"
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

/// The ADR-0008 envelope for `code`: status, `content-type` and code, all asserted, because a test
/// that checks only the status is how a wrong code ships.
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

/// The 202 a delete answers with: the row, and the state the request is about to leave.
fn assert_deleting(response: &Response, id: &str) {
    assert_eq!(response.status, 202, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    assert_eq!(body["id"], id, "{}", response.body);
    assert_eq!(body["status"], "deleting", "{}", response.body);
}

/// The 202's `kept` key (HS-043): the delete's own answer about what the teardown leaves in the
/// repository. Absent is a bug, so this panics rather than answering `None`.
fn kept(response: &Response) -> Value {
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    body.get("kept")
        .cloned()
        .unwrap_or_else(|| panic!("the answer carries no `kept`: {}", response.body))
}

fn row(state: &AppState, id: &str) -> Option<Session> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database.sessions().get(id).expect("the row reads")
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
        row(state, id).map(|session| session.status) == Some(want)
    })
    .await;
}

async fn wait_gone(state: &AppState, id: &str) {
    wait_until("the session's row was removed", || row(state, id).is_none()).await;
}

/// How many committed events the session's log holds.
fn log_len(state: &AppState, id: &str) -> usize {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(id, 0, 500)
        .expect("the log reads")
        .len()
}

/// One state change through the one state function, as the policies that will move a session will.
async fn drive(state: &AppState, id: &str, trigger: Trigger, reason: Option<&str>) {
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, reason)
        .await
        .expect("the move is in the table");
}

/// The calls a delete makes, without the provisioning ones a create's task ran first.
fn lifecycle(stub: &holosmith::backend::StubBackend) -> Vec<Call> {
    stub.calls()
        .into_iter()
        .filter(|call| !matches!(call, Call::Provision(_) | Call::Undo(_)))
        .collect()
}

#[tokio::test]
async fn a_delete_answers_the_deleting_row_and_then_removes_it() {
    let dir = TempDir::new("delete-round-trip");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-round-trip").await);
    wait_status(&state, &id, Status::Idle).await;
    // The row can be ahead of its events (the writer commits on its own tick), so the log is waited
    // for rather than sampled: the count the removal has to take is the count that exists.
    wait_until("the provisioning's events are committed", || {
        log_len(&state, &id) > 0
    })
    .await;
    let before = log_len(&state, &id);
    assert!(
        before > 0,
        "the log was empty before the delete, so nothing would prove it was removed"
    );

    let accepted = delete(served.addr, &id, "").await;
    assert_deleting(&accepted, &id);

    // The removal is a task's, so the row is the thing to wait for — never the answer.
    wait_gone(&state, &id).await;

    // The log goes with the row (ADR-0023): a removed session's transcript is not left behind as
    // rows nothing can read.
    assert_eq!(
        log_len(&state, &id),
        0,
        "{before} events of a removed session survived it"
    );

    // Nothing a client can read names the session any more: one row, the list, and its stream.
    let one = support::fetch(served.addr, &format!("/api/sessions/{id}")).await;
    assert_envelope(&one, 404, NOT_FOUND);
    let list = support::fetch(served.addr, "/api/sessions").await;
    assert_eq!(list.status, 200, "{}", list.body);
    assert!(
        !list.body.contains(&id),
        "the removed session is still listed: {}",
        list.body
    );

    // The backend was asked to remove what it owns, after the snapshot that makes the removal safe.
    assert_eq!(
        lifecycle(&stub),
        [Call::Snapshot, Call::Delete],
        "the snapshot must come before the removal"
    );

    // And the answer carries the keep-rule's answer even when there is nothing to keep (HS-043): a stub
    // session has no repository, so the three keys are explicit nulls rather than absent — a client should
    // not have to tell "no claim" from "nothing survived".
    assert_eq!(
        kept(&accepted),
        serde_json::json!({"branch": null, "wip": null, "recovery": null}),
        "the 202 does not carry the delete's own answer: {}",
        accepted.body
    );
}

#[tokio::test]
async fn the_delete_answer_names_the_work_it_keeps_and_the_command_that_recovers_it() {
    // The story's fourth bullet at the wire (HS-043): the 202 names the branch and the snapshot ref the
    // teardown will leave, with the session's own id in the spec's recovery command — and the names are
    // still in the repository afterwards.
    //
    // The backend is the Docker one over a real repository with **no daemon**: the keep-rule and the
    // worktree teardown are host git, and a container or a network the daemon has forgotten is a fact
    // rather than a failure (ADR-0023), so this is the whole of the path the rule lives on.
    let dir = TempDir::new("delete-keeps-the-work");
    let root = dir.file("projects");
    let repo = root.join("holosmith");
    support::repository_on(&repo, "main");
    let config = Config {
        projects_root: root.clone(),
        max_live_sessions: 2,
        ..Config::default()
    };
    let mut state = state_in(&dir, config);
    state.backend = Backends::for_run(
        Backend::Docker,
        ContainerSettings {
            docker_host: Some("unix:///nonexistent/holosmith-delete.sock".to_string()),
            ..ContainerSettings::at(&root, dir.file("data"))
        },
    );
    let served = Served::start(http::router(state.clone()), GRACE).await;

    // A row with a worktree, written the way the provisioner writes it (ADR-0029) — no create, because
    // this test is about the delete's answer rather than about provisioning.
    let id = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .create(
                &NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: None,
                    base_commit: None,
                },
                holosmith::clock::unix_now(),
            )
            .expect("a session is creatable")
            .id
    };
    let session = row(&state, &id).expect("the row is there");
    let path = match state
        .backend
        .provision(&session, Step::Worktree)
        .await
        .expect("the worktree step works on a real repository")
    {
        Provisioned::Worktree {
            path, base_commit, ..
        } => {
            let database = state.database.lock().expect("the lock is not poisoned");
            database
                .sessions()
                .set_worktree(&id, &path, &base_commit)
                .expect("the row records the worktree");
            std::path::PathBuf::from(path)
        }
        Provisioned::Nothing => panic!("the worktree step recorded nothing"),
    };

    // The agent's work: one commit beyond the base, and one change the delete's snapshot is what saves.
    std::fs::write(path.join("work.txt"), "committed\n").expect("the worktree is writable");
    support::git(&path, &["add", "work.txt"]);
    support::git(&path, &["commit", "--quiet", "-m", "the agent's commit"]);
    std::fs::write(path.join("tracked.txt"), "uncommitted\n").expect("the worktree is writable");

    let accepted = delete(served.addr, &id, "").await;
    assert_deleting(&accepted, &id);
    let answer = kept(&accepted);
    let reference = format!("refs/holosmith/wip/{id}");
    assert_eq!(
        answer["branch"],
        format!("agent/{id}"),
        "the answer does not name the branch it keeps: {}",
        accepted.body
    );
    assert_eq!(
        answer["wip"], reference,
        "the answer does not name the snapshot ref: {}",
        accepted.body
    );
    assert_eq!(
        answer["recovery"],
        format!("git branch rescue {reference}"),
        "the answer does not carry the spec's recovery command: {}",
        accepted.body
    );

    wait_gone(&state, &id).await;

    // What the answer promised is what the repository has — and the worktree it was checked out in is
    // gone, because a kept branch is a name rather than a copy of the files.
    assert!(
        support::branch_exists(&repo, &format!("agent/{id}")),
        "the answer promised the branch and the delete took it"
    );
    let recovered = support::git(&repo, &["rev-parse", "--verify", &reference]);
    assert_eq!(
        recovered.len(),
        40,
        "the snapshot ref was not kept: {recovered}"
    );
    assert!(!path.exists(), "the worktree outlived the delete");
    support::git(&repo, &["branch", "rescue", &reference]);
    assert_eq!(
        support::git(&repo, &["show", "rescue:tracked.txt"]),
        "uncommitted",
        "the recovery command recovers the wrong work"
    );

    served.finish().await.expect("a clean shutdown");
}

#[tokio::test]
async fn a_delete_closes_the_sessions_stream() {
    let dir = TempDir::new("delete-closes-stream");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-closes-stream").await);
    wait_status(&state, &id, Status::Idle).await;

    let mut stream = support::Sse::connect(served.addr, &id, None).await;
    assert_eq!(stream.status, 200, "the stream did not open");

    let accepted = delete(served.addr, &id, "").await;
    assert_deleting(&accepted, &id);
    wait_gone(&state, &id).await;

    // `drain` reads until the server ends the response, and fails the test if it never does: that is
    // the whole assertion. Which frames arrive first is not: the `deleting` event is committed by the
    // writer on its own tick, and the removal does not wait for it.
    let frames = stream.drain(PATIENCE).await;
    assert!(
        frames.len() < 200,
        "the stream of a removed session forwarded {} frames",
        frames.len()
    );

    // A client that reconnects after the close is answered for a row that is gone.
    let again = support::Sse::connect(served.addr, &id, None).await;
    assert_eq!(again.status, 404, "a removed session still has a stream");
}

#[tokio::test]
async fn a_failing_snapshot_refuses_the_delete_and_leaves_the_row_alone() {
    let dir = TempDir::new("delete-unsaved-work");
    let stub = fast_stub().failing_snapshot();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-unsaved-work").await);
    wait_status(&state, &id, Status::Idle).await;

    let refused = delete(served.addr, &id, "").await;
    assert_envelope(&refused, 409, UNSAVED_WORK);

    // The point of refusing: the row is exactly what it was, so the operator can go and commit the
    // work (or force the delete) and nothing has to be undone.
    let session = row(&state, &id).expect("the session is still there");
    assert_eq!(session.status, Status::Idle, "the refusal moved the row");
    assert_eq!(session.status_reason, None);
    // The snapshot was attempted and nothing was removed.
    assert_eq!(lifecycle(&stub), [Call::Snapshot]);
    // …and no `deleting` transition was reported, which is what a client's stream would have shown.
    let deleting_events: Vec<String> = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .events()
            .read_since(&id, 0, 200)
            .expect("the log reads")
            .into_iter()
            .filter(|event| event.payload.contains("\"to\":\"deleting\""))
            .map(|event| event.payload)
            .collect()
    };
    assert!(
        deleting_events.is_empty(),
        "a refused delete reported a transition: {deleting_events:?}"
    );
}

#[tokio::test]
async fn a_forced_delete_removes_the_session_past_a_failed_snapshot() {
    let dir = TempDir::new("delete-forced");
    let stub = fast_stub().failing_snapshot();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-forced").await);
    wait_status(&state, &id, Status::Idle).await;

    let accepted = delete(served.addr, &id, "?force=true").await;
    assert_deleting(&accepted, &id);
    wait_gone(&state, &id).await;

    // `force` forgives the failure; it does not skip the attempt. Spec item 9: the snapshot runs
    // before a delete, and a retry that quietly stopped trying would be a second rule.
    assert_eq!(lifecycle(&stub), [Call::Snapshot, Call::Delete]);
}

#[tokio::test]
async fn a_delete_of_an_unknown_session_is_not_found() {
    let dir = TempDir::new("delete-unknown");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let served = Served::start(http::router(state), GRACE).await;
    let answer = delete(served.addr, "0000000000000000", "").await;
    assert_envelope(&answer, 404, NOT_FOUND);
}

#[tokio::test]
async fn a_second_delete_while_deleting_removes_the_session() {
    let dir = TempDir::new("delete-again");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-again").await);
    wait_status(&state, &id, Status::Idle).await;

    // Where a removal that failed leaves a session. The stub's removal is instant, so the first
    // delete cannot be caught mid-flight; the table's own function puts the row there instead.
    drive(&state, &id, Trigger::Delete, None).await;

    let again = delete(served.addr, &id, "").await;
    assert_deleting(&again, &id);
    wait_gone(&state, &id).await;

    // A repeat request is a retry, not a second delete: no snapshot, and the backend removes the
    // session once.
    assert_eq!(
        lifecycle(&stub),
        [Call::Delete],
        "a repeat delete snapshotted or removed twice"
    );
}

#[tokio::test]
async fn a_delete_after_the_removal_is_not_found() {
    let dir = TempDir::new("delete-idempotent");
    let state = with_stub(state_in(&dir, Config::default()), fast_stub());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-idempotent").await);
    wait_status(&state, &id, Status::Idle).await;

    let accepted = delete(served.addr, &id, "").await;
    assert_deleting(&accepted, &id);
    wait_gone(&state, &id).await;

    // The other half of the idempotency rule, and the deterministic one: while the row exists a
    // repeat is a 202 (above), and once it is gone it is a 404. Never a partial state, never a 500.
    let after = delete(served.addr, &id, "").await;
    assert_envelope(&after, 404, NOT_FOUND);
}

#[tokio::test]
async fn a_delete_ends_a_running_turn_before_it_snapshots() {
    let dir = TempDir::new("delete-running");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, impatient_grace(100)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-running").await);
    wait_status(&state, &id, Status::Idle).await;

    // A running turn nobody will settle. The row is the table's, so the ladder's rung 2 is what ends
    // it — which is the path this test is about: the snapshot must not race an agent still writing.
    drive(&state, &id, Trigger::Prompt, None).await;

    let accepted = delete(served.addr, &id, "").await;
    assert_deleting(&accepted, &id);
    wait_gone(&state, &id).await;

    // The order is the decision: ask the agent, kill the container, snapshot, remove. A snapshot
    // before the turn ended would be a snapshot of a moving tree.
    assert_eq!(
        lifecycle(&stub),
        [
            Call::ClearQueue,
            Call::Abort,
            // `Stopper` snapshots before every stop (HS-042), then the delete takes its own snapshot
            // after the turn ended (the reason the two are separate calls).
            Call::Snapshot,
            Call::Stop,
            Call::Snapshot,
            Call::Delete,
        ]
    );
}

#[tokio::test]
async fn the_ladder_ends_a_turn_without_bringing_the_session_back() {
    // `Ladder::end_turn` is the delete's half of the abort ladder (HS-019), and the difference from
    // `climb` is the whole point: a session that is about to be removed must not be provisioned
    // first. HS-022 and HS-023 climb the same routine, so this is where its contract is pinned.
    let dir = TempDir::new("delete-end-turn");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, impatient_grace(100)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-end-turn").await);
    wait_status(&state, &id, Status::Idle).await;
    drive(&state, &id, Trigger::Prompt, None).await;
    let running = row(&state, &id).expect("the session exists");
    let after_create = stub.calls().len();

    let ending = state.ladder().end_turn(&running, DELETED).await;

    assert_eq!(ending, Ending::Forced, "the hung turn was not forced");
    let stopped = row(&state, &id).expect("the session exists");
    assert_eq!(stopped.status, Status::Stopped);
    assert_eq!(
        stopped.status_reason.as_deref(),
        Some(DELETED),
        "rung 2 records the caller's reason"
    );
    // Nothing was brought back: the calls after the create's provisioning are the ladder's own, and
    // no provisioning step follows them.
    let after_end: Vec<Call> = stub.calls()[after_create..]
        .iter()
        .copied()
        .filter(|call| !matches!(call, Call::Provision(_) | Call::Undo(_)))
        .collect();
    assert_eq!(
        after_end,
        [Call::ClearQueue, Call::Abort, Call::Snapshot, Call::Stop]
    );
    // The other half of "without bringing the session back": no transition leaves `stopped`, so the
    // only event after the forced stop is the stop itself.
    let resumed: Vec<String> = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .events()
            .read_since(&id, 0, 200)
            .expect("the log reads")
            .into_iter()
            .filter(|event| event.payload.contains(r#""from":"stopped""#))
            .map(|event| event.payload)
            .collect()
    };
    assert!(
        resumed.is_empty(),
        "the ladder resumed the session it had just stopped: {resumed:?}"
    );
}

#[tokio::test]
async fn a_failing_snapshot_on_a_running_session_stops_the_turn_and_still_refuses() {
    // The one case where a refusal is *not* harmless, pinned rather than assumed: the spec's order is
    // "abort if running; snapshot", so by the time the snapshot fails the turn is over and rung 2 has
    // written `running -> stopped` with the delete's own reason. The work is lost because it could not
    // be saved, and the row says so — what the operator must *not* get is a 409 that claims nothing
    // happened while the session sits stopped behind it.
    let dir = TempDir::new("delete-running-unsaved");
    let stub = fast_stub().failing_snapshot();
    let state = with_stub(state_in(&dir, impatient_grace(100)), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-running-unsaved").await);
    wait_status(&state, &id, Status::Idle).await;
    drive(&state, &id, Trigger::Prompt, None).await;

    let refused = delete(served.addr, &id, "").await;
    assert_envelope(&refused, 409, UNSAVED_WORK);

    let session = row(&state, &id).expect("the session was not removed");
    assert_eq!(session.status, Status::Stopped, "{}", refused.body);
    assert_eq!(
        session.status_reason.as_deref(),
        Some(DELETED),
        "the row does not say why it stopped"
    );
    // …and nothing was removed: the session is still there, `stopped`, and resumable.
    assert_eq!(
        lifecycle(&stub),
        [
            Call::ClearQueue,
            Call::Abort,
            Call::Snapshot,
            Call::Stop,
            Call::Snapshot,
        ]
    );
}

#[tokio::test]
async fn a_delete_of_a_session_that_is_still_provisioning_removes_it() {
    // The table admits `delete` from every state, so a session caught mid-provision can be removed.
    // The removal is the delete's; the provisioner must not resurrect it (see the step-boundary test
    // below), and the stream must close like any other.
    let dir = TempDir::new("delete-starting");
    let stub = support::slow_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "delete-starting").await);
    // A minute per step: the row is still `starting` when the delete arrives.
    assert_eq!(
        row(&state, &id).map(|session| session.status),
        Some(Status::Starting)
    );

    let accepted = delete(served.addr, &id, "").await;
    assert_deleting(&accepted, &id);
    wait_gone(&state, &id).await;

    // Nothing the endpoint refused: the delete is not a state it has to wait for.
    assert_eq!(lifecycle(&stub), [Call::Snapshot, Call::Delete]);
}

#[tokio::test]
async fn the_provisioner_creates_nothing_for_a_session_that_left_starting() {
    // A delete is legal from `starting` (the table's row is `any + delete`), so the provisioner has
    // to notice that the row it is provisioning is not its own any more: finishing would create
    // resources for a session that is going away and then report a state change the table refuses.
    let dir = TempDir::new("delete-provisioner");
    let stub = fast_stub();
    let state = with_stub(state_in(&dir, Config::default()), stub.clone());
    let starting = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .create(
                &NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: Some("delete-provisioner".to_string()),
                    base_commit: None,
                },
                holosmith::clock::unix_now(),
            )
            .expect("a session is created")
    };
    drive(&state, &starting.id, Trigger::Delete, None).await;

    Provisioner::new(
        state.backend.clone(),
        Arc::clone(&state.database),
        state.events.clone(),
    )
    .run(starting.clone())
    .await;

    assert!(
        stub.calls().is_empty(),
        "the provisioner ran steps for a session that left starting: {:?}",
        stub.calls()
    );
    assert_eq!(
        row(&state, &starting.id)
            .expect("the session exists")
            .status,
        Status::Deleting,
        "the provisioner moved a row it no longer owned"
    );
}

#[tokio::test]
async fn a_delete_without_the_client_header_is_refused_by_the_guard() {
    // The route is the crate's first `DELETE`, so this is the proof that the guard layer applies to
    // it exactly as it does to a prompt: nothing about a new method is special-cased.
    let state = state();
    let served = Served::start(http::router(state), GRACE).await;
    // The origin is right and the client header is missing: the guards check the origin first, so
    // omitting both would prove `bad_origin` instead of the header this test is about.
    let text = "DELETE /api/sessions/0000000000000000 HTTP/1.1\r\nHost: 127.0.0.1\r\n\
                Origin: http://localhost:8080\r\nContent-Type: application/json\r\n\
                Content-Length: 0\r\nConnection: close\r\n\r\n";
    assert_envelope(&request(served.addr, text).await, 403, BAD_CLIENT);
}

#[test]
fn the_delete_route_is_recorded_as_mutating() {
    let state = state();
    let app = http::App::new(http::router(state), Guards::from_config(&Config::default()));
    let entry = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}" && entry.method == Method::DELETE)
        .expect("the delete route is registered");
    assert_eq!(entry.kind, RouteKind::Mutating);
}
