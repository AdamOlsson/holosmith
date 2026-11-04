//! The worktree step against a real repository (HS-025, spec item 1).
//!
//! No Docker and no daemon: the step is host `git`, so these tests build a repository in a `TempDir`
//! and drive the shipped backend over it. The end-to-end case at the bottom is the same step through a
//! create — the provisioner writes the row, the backend makes the files, and the network step's refusal
//! (HS-027's, not this story's) is what makes the provisioner take the worktree back.
//!
//! The conventions the harness keeps: a fresh `TempDir` per test (two `Database::open`s on one path in
//! one process is ADR-0009's trap), `wait_until` rather than `sleep` for anything a background task
//! does, and a real socket rather than a hand-built state wherever a client is involved.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use holosmith::backend::{Backends, DockerBackend, Provisioned, SessionBackend, Step};
use holosmith::config::Config;
use holosmith::http::{self, AppState};
use holosmith::runtime::Backend;
use holosmith::sessions::state::Trigger;
use holosmith::sessions::{Session, Status};
use holosmith::worktree::Worktree;
use serde_json::Value;

mod support;
use support::{PATIENCE, Response, Served, TempDir, head, repository, request, state_in};

/// The drain deadline a test gives `serve`: a socket it forgot to drop cannot hold the server for the
/// production ten seconds.
const GRACE: Duration = Duration::from_millis(100);

const BODY: &str = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;

const ORIGIN: &str = "http://localhost:8080";

/// A session with no row behind it: the shape the provisioner hands a backend.
fn session(id: &str, base_commit: Option<&str>) -> Session {
    Session {
        id: id.to_string(),
        project: "holosmith".to_string(),
        model: "claude-sonnet-4.5".to_string(),
        thinking: "high".to_string(),
        worktree: String::new(),
        base_commit: base_commit.map(str::to_string),
        status: Status::Starting,
        status_reason: None,
        client_key: None,
        network: false,
        created_at: 1_760_000_000,
        last_active_at: 1_760_000_000,
        stopped_at: None,
        usage_in: 0,
        usage_out: 0,
    }
}

/// A projects root inside `dir`, with the repository `holosmith` in it and one commit.
fn projects(dir: &TempDir) -> (PathBuf, PathBuf) {
    let root = dir.file("projects");
    let repo = root.join("holosmith");
    repository(&repo);
    (root, repo)
}

fn backend(root: &Path) -> DockerBackend {
    DockerBackend::new(root)
}

/// The `Provisioned` a worktree step that worked answers, as `(path, base_commit, created)`.
fn made(provisioned: Provisioned) -> (String, String, bool) {
    match provisioned {
        Provisioned::Worktree {
            path,
            base_commit,
            created,
        } => (path, base_commit, created),
        Provisioned::Nothing => panic!("the worktree step recorded nothing"),
    }
}

#[tokio::test]
async fn the_worktree_step_makes_the_branch_and_names_the_commit_it_starts_from() {
    let dir = TempDir::new("worktree-create");
    let (root, repo) = projects(&dir);
    let backend = backend(&root);
    let session = session("0123456789abcdef", None);

    let (path, base_commit, created) = made(
        backend
            .provision(&session, Step::Worktree)
            .await
            .expect("the worktree step works on a repository"),
    );
    assert!(
        created,
        "the first provision did not say it made the worktree"
    );

    // The story's own command, spelled out: `<projects>/.worktrees/<id>` on branch `agent/<id>`.
    assert_eq!(
        path,
        root.join(".worktrees/0123456789abcdef")
            .display()
            .to_string()
    );
    assert_eq!(base_commit, head(&repo), "the branch starts at HEAD");
    assert_eq!(
        support::git(&repo, &["rev-parse", "agent/0123456789abcdef"]),
        base_commit,
        "the branch is not at the base commit the row records"
    );
    assert!(
        Path::new(&path).join(".git").exists(),
        "the worktree has no .git file: the agent would have no repository"
    );
    assert_eq!(
        support::worktrees(&repo).len(),
        2,
        "the repository is not holding exactly one session worktree"
    );
}

#[tokio::test]
async fn a_second_provision_adopts_the_worktree_and_keeps_the_base_the_row_has() {
    // A resume re-provisions: the worktree, the branch and every commit on it are still there, so the
    // step must adopt rather than fail on a branch that already exists — and the base it answers must
    // stay the one the session started from, because `ahead` is measured from there (HS-061).
    // The label carries a space on purpose: `git worktree list --porcelain` prints the resolved path and
    // `registered()` compares it after `canonicalize`, so a projects root with a space in it is the case
    // that would break if either half were wrong.
    let dir = TempDir::new("worktree adopt with a space");
    let (root, repo) = projects(&dir);
    let backend = backend(&root);
    let first = session("0123456789abcdef", None);
    let (path, base_commit, _) = made(
        backend
            .provision(&first, Step::Worktree)
            .await
            .expect("the first provision works"),
    );

    // Two commits in the worktree, the way an agent would leave it.
    let worktree = Path::new(&path);
    support::git(
        worktree,
        &["commit", "--quiet", "--allow-empty", "-m", "second"],
    );
    support::git(
        worktree,
        &["commit", "--quiet", "--allow-empty", "-m", "third"],
    );
    let moved = head(worktree);
    assert_ne!(moved, base_commit);

    let resumed = session("0123456789abcdef", Some(&base_commit));
    let (again_path, again_base, created) = made(
        backend
            .provision(&resumed, Step::Worktree)
            .await
            .expect("a provision over an existing worktree adopts it"),
    );
    assert!(
        !created,
        "an adopted worktree was reported as made: undoing it would delete the session's commits"
    );
    assert_eq!(again_path, path);
    assert_eq!(
        again_base, base_commit,
        "the adopted worktree reported the branch's head instead of the row's base"
    );
    assert_eq!(head(worktree), moved, "adopting must not touch the commits");
    assert_eq!(
        support::worktrees(&repo).len(),
        2,
        "a second worktree appeared"
    );
}

#[tokio::test]
async fn a_session_whose_worktree_is_gone_is_attached_to_its_existing_branch() {
    // The teardown that removes the directory and the one that deletes the branch are two commands, so a
    // crash between them leaves a branch with no worktree. Creating one must then attach that branch
    // rather than fail on `-b`, and the commits on it must survive.
    let dir = TempDir::new("worktree-reattach");
    let (root, repo) = projects(&dir);
    let backend = backend(&root);
    let first = session("0123456789abcdef", None);
    let (path, base_commit, _) = made(
        backend
            .provision(&first, Step::Worktree)
            .await
            .expect("the first provision works"),
    );
    support::git(
        Path::new(&path),
        &["commit", "--quiet", "--allow-empty", "-m", "second"],
    );
    let committed = head(Path::new(&path));
    // The half-teardown: git forgets the worktree, the branch stays.
    support::git(&repo, &["worktree", "remove", "--force", &path]);
    assert_eq!(support::worktrees(&repo).len(), 1);

    let resumed = session("0123456789abcdef", Some(&base_commit));
    let (again_path, again_base, created) = made(
        backend
            .provision(&resumed, Step::Worktree)
            .await
            .expect("a worktree is made again from the branch that survived"),
    );
    assert!(
        !created,
        "a branch that survived a half-teardown was reported as made: undoing it would delete its commits"
    );
    assert_eq!(again_path, path);
    assert_eq!(
        head(Path::new(&path)),
        committed,
        "the branch's commit was lost"
    );
    assert_eq!(again_base, base_commit, "the base moved with the branch");
}

#[tokio::test]
async fn ahead_counts_the_commits_the_branch_has_that_the_base_does_not() {
    // The story's demo, as a number: commit in the worktree, see `ahead` increment.
    let dir = TempDir::new("worktree-ahead");
    let (root, repo) = projects(&dir);
    let worktree = Worktree::of(&root, "holosmith", "0123456789abcdef");
    let base = worktree
        .ensure(None)
        .expect("the worktree is made from the repository's HEAD")
        .base_commit;

    assert_eq!(worktree.ahead(&base).expect("ahead reads"), 0);
    for (count, message) in [(1, "second"), (2, "third")] {
        support::git(
            &worktree.path,
            &["commit", "--quiet", "--allow-empty", "-m", message],
        );
        assert_eq!(
            worktree.ahead(&base).expect("ahead reads"),
            count,
            "ahead did not follow the commits in the worktree"
        );
    }
    // The story's command, spelled the way the implementation spells it.
    assert_eq!(
        support::git(
            &repo,
            &[
                "rev-list",
                "--count",
                &format!("{base}..agent/0123456789abcdef")
            ]
        ),
        "2"
    );
}

#[tokio::test]
async fn undo_removes_the_worktree_the_branch_and_the_registration() {
    let dir = TempDir::new("worktree-undo");
    let (root, repo) = projects(&dir);
    let backend = backend(&root);
    let session = session("0123456789abcdef", None);
    let (path, _, _) = made(
        backend
            .provision(&session, Step::Worktree)
            .await
            .expect("the worktree step works"),
    );

    backend
        .undo(&session, Step::Worktree)
        .await
        .expect("teardown works");

    assert!(
        !Path::new(&path).exists(),
        "the worktree directory is still there: {path}"
    );
    assert!(
        !support::branch_exists(&repo, "agent/0123456789abcdef"),
        "the branch is still there"
    );
    assert_eq!(
        support::worktrees(&repo),
        vec![repo.display().to_string()],
        "the registration was not pruned"
    );
}

#[tokio::test]
async fn deleting_a_session_removes_its_worktree_too() {
    // `delete` is "remove everything the session owns" (HS-019's flow), and from this story the worktree
    // is part of that: a delete that left it behind would leave the files of a session nobody can see.
    let dir = TempDir::new("worktree-delete");
    let (root, repo) = projects(&dir);
    let backend = backend(&root);
    let session = session("0123456789abcdef", None);
    let (path, _, _) = made(
        backend
            .provision(&session, Step::Worktree)
            .await
            .expect("the worktree step works"),
    );

    backend.delete(&session).await.expect("the delete works");

    assert!(
        !Path::new(&path).exists(),
        "the worktree directory is still there"
    );
    assert!(!support::branch_exists(&repo, "agent/0123456789abcdef"));
}

#[tokio::test]
async fn tearing_down_a_worktree_that_never_existed_is_not_a_failure() {
    // "A missing worktree is not an error" — twice, because the delete flow runs the teardown after the
    // provisioner's undo may already have run it, and because a session can be deleted before its
    // provisioning ever reached the worktree step.
    let dir = TempDir::new("worktree-absent");
    let (root, repo) = projects(&dir);
    let backend = backend(&root);
    let session = session("0123456789abcdef", None);

    backend
        .undo(&session, Step::Worktree)
        .await
        .expect("a worktree that was never made is nothing to undo");
    backend
        .delete(&session)
        .await
        .expect("a session whose worktree was never made can still be removed");
    assert_eq!(
        support::worktrees(&repo),
        vec![repo.display().to_string()],
        "the teardown registered something"
    );
}

#[tokio::test]
async fn a_project_that_is_not_a_repository_is_refused_with_its_path() {
    // The reason reaches the row's `status_reason` and the transcript, so it names the step, the
    // directory and what git said: an operator with a mistyped projects_root reads the fix.
    let dir = TempDir::new("worktree-no-repository");
    let root = dir.file("projects");
    std::fs::create_dir_all(&root).expect("the projects root is creatable");
    let backend = backend(&root);

    let error = backend
        .provision(&session("0123456789abcdef", None), Step::Worktree)
        .await
        .expect_err("there is no repository to branch from");

    let sentence = error.to_string();
    assert!(
        sentence.starts_with("the worktree step: "),
        "the sentence does not name the step: {sentence}"
    );
    assert!(
        sentence.contains(&root.join("holosmith").display().to_string()),
        "the sentence does not name the repository's path: {sentence}"
    );
    assert!(
        sentence.contains("git"),
        "the sentence does not say what failed: {sentence}"
    );
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

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn a_create_records_the_worktree_and_takes_it_back_when_the_network_step_refuses() {
    // The provisioner is what writes the row, and the worktree step is the only one that has anything to
    // write. The next step — the session's network — belongs to HS-027 and refuses, so the provisioner
    // undoes what completed (ADR-0015): the row keeps the record, the disk keeps nothing.
    let dir = TempDir::new("worktree-create-flow");
    let (root, repo) = projects(&dir);
    let base = head(&repo);
    let config = Config {
        projects_root: root.clone(),
        max_live_sessions: 1,
        ..Config::default()
    };
    let mut state = state_in(&dir, config);
    state.backend = Backends::for_run(Backend::Docker, root.clone());
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let id = created_id(&post_create(served.addr, "verify-worktree-1").await);

    wait_until("the session left starting", || {
        row(&state, &id).status != Status::Starting
    })
    .await;

    let failed = row(&state, &id);
    assert_eq!(failed.status, Status::Error, "{:?}", failed.status_reason);
    assert_eq!(
        failed.worktree,
        root.join(".worktrees").join(&id).display().to_string(),
        "the row does not record where the worktree was"
    );
    assert_eq!(
        failed.base_commit.as_deref(),
        Some(base.as_str()),
        "the row does not record the commit the branch started from"
    );
    let reason = failed.status_reason.expect("an error carries a reason");
    assert!(
        reason.to_string().contains("HS-027"),
        "the failure is not the network step's: {reason}"
    );
    // Undone: the reason is the *next* step's refusal, so the worktree the first step made is gone.
    assert!(
        !root.join(".worktrees").join(&id).exists(),
        "the refused session left its worktree behind"
    );
    assert!(!support::branch_exists(&repo, &format!("agent/{id}")));

    served.finish().await.expect("a clean shutdown");
}

/// A state whose backend is the Docker one, in a projects root the caller built.
fn docker_state(dir: &TempDir, root: &Path) -> AppState {
    let config = Config {
        projects_root: root.to_path_buf(),
        max_live_sessions: 2,
        ..Config::default()
    };
    let mut state = state_in(dir, config);
    state.backend = Backends::for_run(Backend::Docker, root.to_path_buf());
    state
}

/// One session row, written the way the create handler writes it, with no server in between.
fn create_row(state: &AppState) -> Session {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .create(
            &holosmith::sessions::NewSession {
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
}

#[tokio::test]
async fn a_later_step_failure_does_not_take_an_adopted_worktree_back() {
    // The state a resume meets: the row records the worktree, and the files and the commits on it are
    // still on disk. The worktree step adopts it, the network step refuses, and the provisioner must not
    // undo a step that **made** nothing — the commits are the agent's work, and deleting them to tidy up
    // after a step that failed would be data loss dressed as cleanliness (ADR-0029).
    let dir = TempDir::new("worktree-adopted-undo");
    let (root, repo) = projects(&dir);
    let state = docker_state(&dir, &root);
    let session = create_row(&state);

    // The first provision: it makes the worktree, the network step refuses, and the undo takes the
    // worktree back with it.
    state.provisioner().run(session.clone()).await;
    let failed = row(&state, &session.id);
    assert_eq!(failed.status, Status::Error, "{:?}", failed.status_reason);
    let base = failed
        .base_commit
        .clone()
        .expect("the worktree step recorded its base");
    let path = root.join(".worktrees").join(&session.id);
    assert!(
        !path.exists(),
        "the first provision's undo left the worktree"
    );

    // What a crashed or resumed session looks like: the worktree is there again, with a commit on it and
    // the row's record intact.
    Worktree::of(&root, &session.project, &session.id)
        .ensure(Some(&base))
        .expect("the worktree is made again");
    support::git(
        &path,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "the agent's work",
        ],
    );
    let committed = head(&path);
    {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .set_worktree(&session.id, &path.display().to_string(), &base)
            .expect("the row records the worktree again");
    }
    // `error → starting` is the resume's own row (the table's), which is what the provisioner waits for.
    state
        .transitioner()
        .apply_logged(&session.id, Trigger::Resume, None)
        .await;
    let resumed = row(&state, &session.id);
    assert_eq!(resumed.status, Status::Starting);

    state.provisioner().run(resumed).await;

    let after = row(&state, &session.id);
    assert_eq!(
        after.status,
        Status::Error,
        "the network step refused again"
    );
    assert!(
        path.exists(),
        "the adopted worktree was removed when the next step refused"
    );
    assert_eq!(
        head(&path),
        committed,
        "the agent's commit was lost with the adopted worktree"
    );
    assert!(
        support::branch_exists(&repo, &format!("agent/{}", session.id)),
        "the adopted worktree's branch was deleted when the next step refused"
    );

    // And a delete still removes it: "an adopted worktree is not this provision's to undo" is not "a
    // delete cannot remove it".
    state
        .backend
        .delete(&row(&state, &session.id))
        .await
        .expect("the delete removes what the session owns");
    assert!(!path.exists(), "the delete left the worktree behind");
}
