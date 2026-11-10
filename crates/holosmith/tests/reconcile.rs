//! The startup reconcile (HS-044, V9): what a `kill -9` leaves behind, put right before anybody is served.
//!
//! These tests are the daemon-free half, and deliberately so: the row a crash leaves is a *row*, and the
//! sweep's repository rules are host `git`. A machine without Docker is a supported machine (a hub), and it
//! must be able to prove that a `running` row comes back, that a `starting` one is resumed or fails with a
//! sentence, that a delete's row is left alone, and that a worktree no row owns goes while its branch and its
//! snapshot ref stay (HS-043, ADR-0052). The container half is the gate's `case_reconcile_orphans` and the
//! story's demo, on a real daemon.
//!
//! The crash itself is a **write**, not a kill: `set_status` is the state the process died in, and that is the
//! only thing the reconcile can see.

use std::path::{Path, PathBuf};
use std::time::Duration;

use holosmith::backend::{Backends, DockerBackend};
use holosmith::config::Config;
use holosmith::container::ContainerSettings;
use holosmith::events::Event;
use holosmith::http::AppState;
use holosmith::reconcile::{self, INTERRUPTED};
use holosmith::runtime::{Backend, Runtime};
use holosmith::sessions::{NewSession, Session, Status};
use holosmith::worktree::Worktree;

mod support;
use support::{PATIENCE, TempDir, fast_stub, git, repository, state_in, with_stub};

/// A socket no daemon answers on: the Docker backend's worktree step still runs real git, and the sweep's
/// listings fail with a `warn` instead of touching this machine's containers.
const NO_DAEMON: &str = "unix:///nonexistent/holosmith-test.sock";

const NOW: i64 = 1_760_000_000;

/// A config whose projects root is `root` and whose daemon is nobody's.
fn config(root: &Path) -> Config {
    Config {
        projects_root: root.to_path_buf(),
        docker_host: Some(NO_DAEMON.to_string()),
        ..Config::default()
    }
}

/// A state whose projects root is `root`, on the fast stub: the row half is every backend's, and the fast
/// stub is what lets a re-provision finish while the test waits.
fn stub_state(dir: &TempDir, root: &Path) -> AppState {
    with_stub(state_in(dir, config(root)), fast_stub())
}

/// The same state on the Docker backend with no daemon behind it: the worktree step is real, the network step
/// refuses, and nothing on this machine is touched.
fn docker_state(dir: &TempDir, root: &Path) -> AppState {
    let mut state = state_in(dir, config(root));
    let settings = ContainerSettings {
        docker_host: Some(NO_DAEMON.to_string()),
        ..ContainerSettings::at(root, root.join("data"))
    };
    state.runtime = Runtime::started(Backend::Docker);
    state.backend = Backends::Docker(Box::new(DockerBackend::new(settings)));
    state
}

/// A row for `project`, created the way a create creates one.
fn create(state: &AppState, project: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .create(
            &NewSession {
                project: project.to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: None,
                base_commit: None,
            },
            NOW,
        )
        .expect("a session is created")
}

/// Put the row in the state the process died in. The one write no production code may make outside
/// `Transitioner::apply`, and the only way a test can be the crash.
fn force(state: &AppState, id: &str, status: Status) {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .set_status(id, status, None, NOW + 1)
        .expect("the row is written")
        .expect("the session exists");
}

fn row(state: &AppState, id: &str) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the session exists")
}

/// The committed log of one session, oldest first.
fn log(state: &AppState, id: &str) -> Vec<Event> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(id, 0, u32::MAX)
        .expect("the log reads")
}

fn counter(state: &AppState, id: &str) -> i64 {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .last_seq(id)
        .expect("the counter reads")
        .expect("the session exists")
}

/// Wait for a payload to be **committed**: a row is written before its event is (the writer's 25 ms tick),
/// so "the row says idle" is not "the log holds the transition" (HS-018's lesson).
async fn wait_for_payload(state: &AppState, id: &str, payload: &str) {
    wait_until("the transition was committed", || {
        log(state, id).iter().any(|event| event.payload == payload)
    })
    .await;
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

/// A projects root with one commit in `holosmith`, the repository a session's branch lives in.
fn project(dir: &TempDir) -> PathBuf {
    let repo = dir.file("projects/holosmith");
    repository(&repo);
    repo
}

#[tokio::test]
async fn a_row_that_was_running_comes_back_idle_and_says_the_turn_was_interrupted() {
    let dir = TempDir::new("reconcile-running");
    let root = dir.file("projects");
    std::fs::create_dir_all(&root).expect("the projects root is creatable");
    let state = stub_state(&dir, &root);
    let session = create(&state, "holosmith");
    force(&state, &session.id, Status::Running);

    state.reconciler().run().await;

    // The row is `idle` — the spec's "recovery on startup → starting, then idle" — and it keeps the turn's
    // word, which is what the page's badge reads.
    wait_until("the session came back idle", || {
        row(&state, &session.id).status == Status::Idle
    })
    .await;
    let back = row(&state, &session.id);
    assert_eq!(back.status_reason.as_deref(), Some(INTERRUPTED));
    assert!(
        Status::ALL
            .iter()
            .all(|status| status.as_str() != INTERRUPTED),
        "interrupted became a state"
    );

    let interrupted =
        format!(r#"{{"from":"running","to":"starting","step":null,"reason":"{INTERRUPTED}"}}"#);
    let settled =
        format!(r#"{{"from":"starting","to":"idle","step":null,"reason":"{INTERRUPTED}"}}"#);
    wait_for_payload(&state, &session.id, &interrupted).await;
    wait_for_payload(&state, &session.id, &settled).await;
    let log = log(&state, &session.id);
    let event = log
        .iter()
        .find(|event| event.payload == interrupted)
        .expect("the crash transition is in the log");
    assert_eq!(event.kind, "session.status");
}

#[tokio::test]
async fn an_idle_row_is_restarted_with_no_reason_at_all() {
    let dir = TempDir::new("reconcile-idle");
    let root = dir.file("projects");
    std::fs::create_dir_all(&root).expect("the projects root is creatable");
    let state = stub_state(&dir, &root);
    let session = create(&state, "holosmith");
    // A create's row is `starting`; the crash left a finished provision behind it.
    force(&state, &session.id, Status::Idle);

    state.reconciler().run().await;

    wait_until("the session came back idle", || {
        row(&state, &session.id).status == Status::Idle
    })
    .await;
    assert_eq!(
        row(&state, &session.id).status_reason,
        None,
        "an idle session claims a turn was interrupted though none was running"
    );
    // It *was* moved through `starting` and provisioned again: the container it named died with the process.
    wait_for_payload(
        &state,
        &session.id,
        r#"{"from":"idle","to":"starting","step":null,"reason":null}"#,
    )
    .await;
    wait_for_payload(
        &state,
        &session.id,
        r#"{"from":"starting","to":"idle","step":null,"reason":null}"#,
    )
    .await;
}

#[tokio::test]
async fn a_row_stuck_in_starting_is_resumed_and_never_left_dangling() {
    let dir = TempDir::new("reconcile-starting");
    let root = dir.file("projects");
    std::fs::create_dir_all(&root).expect("the projects root is creatable");
    let state = stub_state(&dir, &root);
    let session = create(&state, "holosmith");
    assert_eq!(row(&state, &session.id).status, Status::Starting);

    state.reconciler().run().await;

    wait_until("the starting row reached idle", || {
        row(&state, &session.id).status == Status::Idle
    })
    .await;
}

#[tokio::test]
async fn a_starting_row_whose_project_is_gone_ends_error_with_a_sentence() {
    let dir = TempDir::new("reconcile-gone");
    // The projects root exists and is empty: `holosmith` is a project the machine no longer holds.
    let root = dir.file("projects");
    std::fs::create_dir_all(&root).expect("the projects root is creatable");
    let state = docker_state(&dir, &root);
    let session = create(&state, "holosmith");

    state.reconciler().run().await;

    wait_until("the starting row stopped being starting", || {
        row(&state, &session.id).status != Status::Starting
    })
    .await;
    let failed = row(&state, &session.id);
    assert_eq!(
        failed.status,
        Status::Error,
        "a project that cannot be adopted did not fail the row: {failed:?}"
    );
    let reason = failed.status_reason.unwrap_or_default();
    assert!(
        reason.contains("worktree"),
        "the reason does not name the step that failed: {reason}"
    );
}

#[tokio::test]
async fn the_rows_a_crash_cannot_own_are_left_exactly_where_they_were() {
    let dir = TempDir::new("reconcile-untouched");
    let root = dir.file("projects");
    std::fs::create_dir_all(&root).expect("the projects root is creatable");
    let state = stub_state(&dir, &root);
    let stopped = create(&state, "holosmith");
    force(&state, &stopped.id, Status::Stopped);
    let failed = create(&state, "holosmith");
    force(&state, &failed.id, Status::Error);
    let deleting = create(&state, "holosmith");
    force(&state, &deleting.id, Status::Deleting);

    state.reconciler().run().await;
    // Nothing is spawned for them, so there is no event to wait for: a tick of the writer is the grace the
    // other tests' waits would have given a provisioner.
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(row(&state, &stopped.id).status, Status::Stopped);
    assert_eq!(row(&state, &failed.id).status, Status::Error);
    assert_eq!(
        row(&state, &deleting.id).status,
        Status::Deleting,
        "the reconcile finished somebody's delete"
    );
    for id in [&stopped.id, &failed.id, &deleting.id] {
        assert!(
            !log(&state, id)
                .iter()
                .any(|event| event.kind == "session.status"),
            "a row the reconcile does not own was reported changed"
        );
    }
}

#[tokio::test]
async fn a_recovery_never_resets_the_seq_counter() {
    let dir = TempDir::new("reconcile-seq");
    let root = dir.file("projects");
    std::fs::create_dir_all(&root).expect("the projects root is creatable");
    let state = stub_state(&dir, &root);
    let session = create(&state, "holosmith");
    force(&state, &session.id, Status::Running);
    // Two events before the crash, so "the counter did not restart at 1" is a fact rather than a hope: the
    // writer commits on a 25 ms tick, so this waits for them.
    state
        .events
        .append(holosmith::events::NewEvent {
            session_id: session.id.clone(),
            kind: "turn.start".to_string(),
            payload: "{}".to_string(),
            created_at: NOW,
        })
        .await
        .expect("the writer accepts an event");
    wait_until("the pre-crash event was committed", || {
        log(&state, &session.id).len() == 1
    })
    .await;
    let before = counter(&state, &session.id);

    state.reconciler().run().await;

    wait_until("the session came back idle", || {
        row(&state, &session.id).status == Status::Idle
    })
    .await;
    // The settle is the recovery's last event, so waiting for it is waiting for everything before it too.
    wait_for_payload(
        &state,
        &session.id,
        &format!(r#"{{"from":"starting","to":"idle","step":null,"reason":"{INTERRUPTED}"}}"#),
    )
    .await;
    assert!(
        counter(&state, &session.id) > before,
        "the counter did not move with the recovery's events"
    );

    let seqs: Vec<i64> = log(&state, &session.id)
        .iter()
        .map(|event| event.seq)
        .collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as i64).collect::<Vec<i64>>(),
        "the log has a duplicate or a gap: {seqs:?}"
    );
    assert_eq!(
        counter(&state, &session.id),
        seqs.len() as i64,
        "last_seq and the highest committed seq disagree"
    );
}

/// The sweep's worktree half on a real repository: an orphan directory goes, its registration goes, and the
/// two names a delete keeps survive (HS-043, ADR-0052).
#[tokio::test]
async fn an_orphan_worktree_goes_and_its_branch_and_snapshot_ref_stay() {
    let dir = TempDir::new("reconcile-worktree");
    let repo = project(&dir);
    let root = dir.file("projects");
    let id = "0123456789abcdef";
    let worktree = Worktree::of(&root, "holosmith", id);
    worktree.ensure(None).expect("the worktree is made");
    // A commit the branch holds and nothing else does, and the snapshot ref a turn takes.
    git(
        &worktree.path,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "the agent's work",
        ],
    );
    let wip = format!("refs/holosmith/wip/{id}");
    git(&repo, &["update-ref", &wip, "HEAD"]);
    let snapshot = git(&repo, &["rev-parse", &wip]);

    let swept = reconcile::sweep_worktrees(&root, &[id.to_string()]);

    assert_eq!(swept.failures, vec![], "a sweep reported a failure");
    assert!(
        !worktree.path.exists(),
        "the orphan directory is still there"
    );
    assert_eq!(
        support::worktrees(&repo),
        vec![repo.display().to_string()],
        "git still registers the orphan"
    );
    assert!(
        support::branch_exists(&repo, &format!("agent/{id}")),
        "the sweep deleted the branch, which is the agent's work"
    );
    assert_eq!(
        git(&repo, &["rev-parse", "--verify", &wip]),
        snapshot,
        "the sweep moved or deleted the snapshot ref"
    );
}

#[tokio::test]
async fn a_directory_git_never_registered_goes_too() {
    let dir = TempDir::new("reconcile-unregistered");
    let repo = project(&dir);
    let root = dir.file("projects");
    let id = "fedcba9876543210";
    // What a killed start can leave: the directory, before git ever heard of it.
    let directory = root.join(holosmith::worktree::WORKTREES).join(id);
    std::fs::create_dir_all(&directory).expect("the orphan directory is creatable");
    std::fs::write(directory.join("a-file"), "half-written").expect("the file writes");

    let swept = reconcile::sweep_worktrees(&root, &[id.to_string()]);

    assert_eq!(swept.failures, vec![], "a sweep reported a failure");
    assert!(!directory.exists());
    assert_eq!(
        support::worktrees(&repo),
        vec![repo.display().to_string()],
        "the repository gained a registration"
    );
}

#[tokio::test]
async fn a_worktree_a_row_owns_is_not_an_orphan() {
    let dir = TempDir::new("reconcile-owned");
    let root = dir.file("projects");
    std::fs::create_dir_all(root.join(holosmith::worktree::WORKTREES))
        .expect("the root is creatable");
    let state = stub_state(&dir, &root);
    let session = create(&state, "holosmith");
    let owned = root.join(holosmith::worktree::WORKTREES).join(&session.id);
    std::fs::create_dir_all(&owned).expect("the directory is creatable");

    // The id is a session's own, whatever its state: only "no row" makes a directory an orphan.
    for status in [
        Status::Starting,
        Status::Idle,
        Status::Stopped,
        Status::Deleting,
    ] {
        force(&state, &session.id, status);
        let rows = {
            let database = state.database.lock().expect("the lock is not poisoned");
            database.sessions().list(None).expect("the rows read")
        };
        assert!(
            reconcile::orphan_worktrees(&root, &rows).is_empty(),
            "the sweep would take a worktree of a row that is {status}"
        );
    }
    assert!(owned.exists());
}
