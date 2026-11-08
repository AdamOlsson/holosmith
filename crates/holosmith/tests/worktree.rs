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
use holosmith::container::ContainerSettings;
use holosmith::http::{self, AppState};
use holosmith::runtime::Backend;
use holosmith::sessions::state::Trigger;
use holosmith::sessions::{Session, Status};
use holosmith::worktree::{Refusal, Retained, Worktree, repository_of};
use serde_json::Value;

mod support;
use support::{PATIENCE, Response, Served, TempDir, head, repository, request, state_in};

/// The drain deadline a test gives `serve`: a socket it forgot to drop cannot hold the server for the
/// production ten seconds.
const GRACE: Duration = Duration::from_millis(100);

/// A `.gitmodules` with one entry: the declaration of a submodule, whether or not it is initialised.
const GITMODULES: &str = "[submodule \"dep\"]\n\tpath = dep\n\turl = ../dep\n";

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

/// A socket no daemon answers on.
///
/// These tests are about where files go on disk, not about a machine that can run containers, and from
/// HS-028 on the Docker backend really does create networks and containers — against a real daemon, in a
/// `TempDir` the daemon cannot even see. An absent socket makes the network step fail deterministically,
/// which is what every one of them asserts (that a later step's refusal takes back what this one made).
const NO_DAEMON: &str = "unix:///nonexistent/holosmith-test.sock";

/// The machine's container settings with no daemon behind them, for `root`.
fn settings(root: &Path) -> ContainerSettings {
    ContainerSettings {
        docker_host: Some(NO_DAEMON.to_string()),
        ..ContainerSettings::at(root, root.join("data"))
    }
}

fn backend(root: &Path) -> DockerBackend {
    DockerBackend::new(settings(root))
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

    backend
        .delete(&session, &Retained::nothing())
        .await
        .expect("the delete works");

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
        .delete(&session, &Retained::nothing())
        .await
        .expect("a session whose worktree was never made can still be removed");
    assert_eq!(
        support::worktrees(&repo),
        vec![repo.display().to_string()],
        "the teardown registered something"
    );
}

// ---- what a delete keeps (HS-043, spec item 9) -------------------------------------------------

/// The two names `[merge] targets` carries by default: the branches a session's work may land in.
fn default_targets() -> Vec<String> {
    targets(&["main", "master"])
}

fn targets(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// A projects root whose repository is on `branch`, with one commit — so `main` and `master` are a
/// decision the test makes rather than this machine's `init.defaultBranch`.
fn projects_on(dir: &TempDir, branch: &str) -> (PathBuf, PathBuf) {
    let root = dir.file("projects");
    let repo = root.join("holosmith");
    support::repository_on(&repo, branch);
    (root, repo)
}

/// The worktree a `Step::Worktree` provision made, with the base commit recorded on `session` the way the
/// provisioner records it (ADR-0029) — `retained` reads the row's, not the branch's.
async fn provisioned_worktree(backend: &DockerBackend, session: &mut Session) -> (PathBuf, String) {
    let (path, base, _) = made(
        backend
            .provision(session, Step::Worktree)
            .await
            .expect("the worktree step works"),
    );
    session.base_commit = Some(base.clone());
    (PathBuf::from(path), base)
}

/// One commit in the session's worktree — the agent's own work, simulated.
fn commit_in(worktree: &Path, name: &str) {
    std::fs::write(worktree.join(name), "the agent's work\n").expect("the worktree is writable");
    support::git(worktree, &["add", name]);
    support::git(worktree, &["commit", "--quiet", "-m", "the agent's commit"]);
}

#[tokio::test]
async fn a_branch_ahead_of_its_base_and_not_merged_survives_a_delete() {
    // The story's first keep-rule: the branch holds work nothing else has, so the delete removes the
    // worktree and leaves the branch behind (spec item 9).
    let dir = TempDir::new("worktree-keep-ahead");
    let (root, repo) = projects_on(&dir, "main");
    let backend = backend(&root);
    let mut session = session("0123456789abcdef", None);
    let (path, _) = provisioned_worktree(&backend, &mut session).await;
    commit_in(&path, "work.txt");

    let retained = backend
        .retained(&session, &default_targets(), None)
        .await
        .expect("the repository answers");
    assert_eq!(
        retained,
        Retained::Known {
            branch: Some("agent/0123456789abcdef".to_string()),
            wip: None,
        },
        "a commit ahead of the base and in no target is work to keep"
    );
    assert_eq!(
        retained.recovery(),
        None,
        "nothing was snapshotted, so there is nothing to recover"
    );

    backend
        .delete(&session, &retained)
        .await
        .expect("the delete works");

    assert!(
        support::branch_exists(&repo, "agent/0123456789abcdef"),
        "the branch's work was deleted with the worktree"
    );
    assert!(!path.exists(), "the worktree is still there");
}

#[tokio::test]
async fn a_branch_merged_into_a_target_does_not_survive_a_delete() {
    // The other half of the first rule: work that is already in a target is not work at risk.
    let dir = TempDir::new("worktree-keep-merged");
    let (root, repo) = projects_on(&dir, "main");
    let backend = backend(&root);
    let mut session = session("0123456789abcdef", None);
    let (path, _) = provisioned_worktree(&backend, &mut session).await;
    commit_in(&path, "work.txt");
    // The merge that makes the branch's work the target's — a fast-forward, which is what `git merge`
    // does while the target has not moved.
    support::git(&repo, &["merge", "--ff-only", "agent/0123456789abcdef"]);

    let retained = backend
        .retained(&session, &default_targets(), None)
        .await
        .expect("the repository answers");
    assert_eq!(
        retained,
        Retained::Known {
            branch: None,
            wip: None,
        },
        "a merged branch holds nothing the target does not"
    );

    backend
        .delete(&session, &retained)
        .await
        .expect("the delete works");

    assert!(
        !support::branch_exists(&repo, "agent/0123456789abcdef"),
        "the merged branch was kept"
    );
}

#[tokio::test]
async fn a_branch_with_nothing_beyond_its_base_does_not_survive_a_delete() {
    // A session that was provisioned and never worked: there is no commit the base does not have, so
    // there is nothing to keep and nothing to say. The repository is on `trunk`, which is **none** of the
    // targets, so the "no target can answer" rule would keep the branch — this test is what says the
    // ahead question is asked first and answers on its own.
    let dir = TempDir::new("worktree-keep-nothing");
    let (root, repo) = projects_on(&dir, "trunk");
    let backend = backend(&root);
    let mut session = session("0123456789abcdef", None);
    let (path, _) = provisioned_worktree(&backend, &mut session).await;

    let retained = backend
        .retained(&session, &default_targets(), None)
        .await
        .expect("the repository answers");
    assert_eq!(retained, Retained::nothing());

    backend
        .delete(&session, &retained)
        .await
        .expect("the delete works");

    assert!(!support::branch_exists(&repo, "agent/0123456789abcdef"));
    assert!(!path.exists(), "the worktree is still there");
}

#[tokio::test]
async fn a_repository_with_only_one_of_the_targets_keeps_an_unmerged_branch() {
    // `[merge] targets` defaults to both `main` and `master` because a machine has one of them, so the
    // absent name must not answer "merged" — and a repository with none of them must not answer at all.
    let dir = TempDir::new("worktree-keep-master");
    let (root, repo) = projects_on(&dir, "master");
    let backend = backend(&root);
    let mut session = session("0123456789abcdef", None);
    let (path, _) = provisioned_worktree(&backend, &mut session).await;
    commit_in(&path, "work.txt");
    let kept = Retained::Known {
        branch: Some("agent/0123456789abcdef".to_string()),
        wip: None,
    };

    // `main` does not exist here and `master` does not have the branch.
    assert_eq!(
        backend
            .retained(&session, &default_targets(), None)
            .await
            .expect("the repository answers"),
        kept,
        "the missing `main` swallowed the branch"
    );
    // A list this repository has none of cannot answer, and an unanswerable question keeps.
    assert_eq!(
        backend
            .retained(&session, &targets(&["trunk"]), None)
            .await
            .expect("the repository answers"),
        kept,
        "a repository with no target at all guessed the branch away"
    );

    backend
        .delete(&session, &kept)
        .await
        .expect("the delete works");
    assert!(
        support::branch_exists(&repo, "agent/0123456789abcdef"),
        "the branch was deleted on a repository that only has `master`"
    );
}

#[tokio::test]
async fn a_snapshot_that_differs_from_the_branch_is_kept_and_named() {
    // The story's second keep-rule: uncommitted work at `refs/holosmith/wip/<id>` survives the delete, the
    // answer names it, and the spec's recovery command is the one that works.
    let dir = TempDir::new("worktree-keep-wip");
    let (root, repo) = projects_on(&dir, "main");
    let backend = backend(&root);
    let mut session = session("0123456789abcdef", None);
    let (path, _) = provisioned_worktree(&backend, &mut session).await;
    std::fs::write(path.join("tracked.txt"), "uncommitted\n").expect("the worktree is writable");
    backend
        .snapshot(&session)
        .await
        .expect("the snapshot is taken");

    let reference = format!("refs/holosmith/wip/{}", session.id);
    let retained = backend
        .retained(&session, &default_targets(), None)
        .await
        .expect("the repository answers");
    assert_eq!(
        retained,
        Retained::Known {
            branch: None,
            wip: Some(reference.clone()),
        }
    );
    assert_eq!(
        retained.recovery(),
        Some(format!("git branch rescue {reference}")),
        "the command is not the spec's"
    );

    backend
        .delete(&session, &retained)
        .await
        .expect("the delete works");

    // The ref is not the teardown's to delete, so it answers afterwards — and the command recovers the
    // work it names, which is what makes the sentence worth printing.
    let commit = support::git(&repo, &["rev-parse", "--verify", &reference]);
    assert_eq!(
        commit.len(),
        40,
        "the snapshot ref did not survive: {commit}"
    );
    support::git(&repo, &["branch", "rescue", &reference]);
    assert_eq!(
        support::git(&repo, &["show", "rescue:tracked.txt"]),
        "uncommitted"
    );
}

#[tokio::test]
async fn a_snapshot_the_branch_already_holds_is_not_named() {
    // A snapshot is a commit of the *working tree*. Once the branch has committed exactly that tree there
    // is nothing left to recover, and a command that recovers nothing is a sentence that misleads.
    let dir = TempDir::new("worktree-keep-equal");
    let (root, _repo) = projects_on(&dir, "main");
    let backend = backend(&root);
    let mut session = session("0123456789abcdef", None);
    let (path, _) = provisioned_worktree(&backend, &mut session).await;
    std::fs::write(path.join("tracked.txt"), "committed after all\n").expect("writable");
    backend
        .snapshot(&session)
        .await
        .expect("the snapshot is taken");
    // The agent then commits exactly what the snapshot captured.
    support::git(&path, &["add", "tracked.txt"]);
    support::git(&path, &["commit", "--quiet", "-m", "the agent's commit"]);

    let retained = backend
        .retained(&session, &default_targets(), None)
        .await
        .expect("the repository answers");
    assert_eq!(
        retained,
        Retained::Known {
            branch: Some("agent/0123456789abcdef".to_string()),
            wip: None,
        },
        "a tree the branch already has was offered as recoverable work"
    );
    assert_eq!(retained.recovery(), None);
}

#[tokio::test]
async fn a_repository_that_cannot_answer_keeps_the_branch() {
    // `Retained::Unknown` is what a row with no base commit (and a repository git cannot read) answers.
    // "Cannot tell" must never mean "delete": the teardown leaves the branch and the answer claims
    // nothing.
    let dir = TempDir::new("worktree-keep-unknown");
    let (root, repo) = projects_on(&dir, "main");
    let backend = backend(&root);
    let mut session = session("0123456789abcdef", None);
    let (path, base) = provisioned_worktree(&backend, &mut session).await;
    commit_in(&path, "work.txt");
    // A row an older binary wrote: the branch and its commit are there, the base is not recorded.
    session.base_commit = None;
    let reference = base;
    assert_eq!(reference.len(), 40, "the provision recorded a commit");

    let retained = backend
        .retained(&session, &default_targets(), None)
        .await
        .expect("the repository answers");
    assert_eq!(retained, Retained::Unknown);
    assert!(
        retained.keeps_branch(),
        "an unanswered question deleted the branch"
    );

    backend
        .delete(&session, &retained)
        .await
        .expect("the delete works");

    assert!(
        support::branch_exists(&repo, "agent/0123456789abcdef"),
        "the branch was deleted because a row had no base commit"
    );
    assert!(!path.exists(), "the worktree is still there");
}

#[tokio::test]
async fn a_repository_that_is_not_there_keeps_nothing_and_claims_nothing() {
    // A projects root with no project in it: nothing of this session's is there, and teardown of
    // something absent is not a failure (ADR-0023).
    let dir = TempDir::new("worktree-keep-absent");
    let root = dir.file("projects");
    let backend = backend(&root);
    let session = session(
        "0123456789abcdef",
        Some("0123456789012345678901234567890123456789"),
    );

    let retained = backend
        .retained(&session, &default_targets(), None)
        .await
        .expect("a missing repository answers");
    assert_eq!(retained, Retained::nothing());
    assert_eq!(retained.recovery(), None);
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

/// One create for `project`, the way the app's own client sends it (HS-026).
async fn post_create_for(addr: SocketAddr, project: &str, key: &str) -> Response {
    let body =
        format!(r#"{{"project":"{project}","model":"claude-sonnet-4.5","thinking":"high"}}"#);
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nIdempotency-Key: {key}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    request(addr, &text).await
}

/// One create, the way the app's own client sends it.
async fn post_create(addr: SocketAddr, key: &str) -> Response {
    post_create_for(addr, "holosmith", key).await
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
    // write. The next step — the session's network — is real from HS-028 and has no daemon to talk to here,
    // so it fails, and the provisioner undoes what completed (ADR-0015): the row keeps the record, the disk
    // keeps nothing.
    let dir = TempDir::new("worktree-create-flow");
    let (root, repo) = projects(&dir);
    let base = head(&repo);
    let config = Config {
        projects_root: root.clone(),
        max_live_sessions: 1,
        ..Config::default()
    };
    let mut state = state_in(&dir, config);
    state.backend = Backends::for_run(Backend::Docker, settings(&root));
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
        reason.to_string().contains("the network step"),
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
    state.backend = Backends::for_run(Backend::Docker, settings(root));
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
        .delete(&row(&state, &session.id), &Retained::nothing())
        .await
        .expect("the delete removes what the session owns");
    assert!(!path.exists(), "the delete left the worktree behind");
}

// ---- the project's refusals (HS-026, spec item 1) ---------------------------------------------
//
// The three rules that say a repository must not be mounted at all. They are read on the create's own
// path, before any row exists, so each test here asserts two things: which code the operator gets, and
// that the refusal left nothing behind.

/// `refuse`'s answer as a value a test can compare, or `None` when the repository may be mounted.
fn refused(repo: &Path) -> Option<Refusal> {
    holosmith::worktree::refuse(repo).err()
}

/// The rule that names a repository is one rule: the create's `refuse` and `Worktree::of` agree.
#[test]
fn a_plain_checkout_is_not_refused() {
    let dir = TempDir::new("refusal-plain");
    let (root, repo) = projects(&dir);
    assert_eq!(refused(&repo), None, "a plain checkout was refused");
    assert_eq!(
        repository_of(&root, "holosmith"),
        repo,
        "the create and the worktree name the repository differently"
    );
}

#[test]
fn a_git_file_is_refused_because_it_is_not_a_directory() {
    // The shape a linked worktree or a submodule checkout has: the repository lives somewhere this mount
    // would not cover, so the hooks and config the spec protects would be elsewhere on disk.
    let dir = TempDir::new("refusal-git-file");
    let (_, repo) = projects(&dir);
    std::fs::rename(repo.join(".git"), repo.join("moved.git"))
        .expect("the repository's .git is movable");
    std::fs::write(repo.join(".git"), "gitdir: moved.git\n").expect("a .git file is writable");

    assert_eq!(refused(&repo), Some(Refusal::NotADirectory));
}

#[test]
fn a_project_that_is_not_there_is_refused_as_not_a_directory() {
    // A directory the projects root does not hold has no `.git` either, and it is one sentence: "there
    // is no plain checkout here". The provisioner never sees it, so no row is written for it either.
    let dir = TempDir::new("refusal-missing");
    let root = dir.file("projects");
    std::fs::create_dir_all(&root).expect("the projects root is creatable");

    assert_eq!(
        refused(&root.join("holosmith")),
        Some(Refusal::NotADirectory)
    );
}

#[test]
fn a_repository_with_submodules_is_refused_in_both_of_its_shapes() {
    // Two facts, one refusal. `.gitmodules` is the declaration — an agent inside the mount can run
    // `submodule update --init` and make the hooks itself — and `.git/modules` is where those hooks then
    // live, which is the hole spec item 1 names.
    let declared = TempDir::new("refusal-gitmodules");
    let (_, repo) = projects(&declared);
    std::fs::write(repo.join(".gitmodules"), GITMODULES).expect(".gitmodules is writable");
    assert_eq!(refused(&repo), Some(Refusal::SubmodulesPresent));

    let initialised = TempDir::new("refusal-git-modules");
    let (_, repo) = projects(&initialised);
    std::fs::create_dir_all(repo.join(".git/modules/dep/hooks"))
        .expect("the modules directory is creatable");
    assert_eq!(refused(&repo), Some(Refusal::SubmodulesPresent));
}

#[test]
fn extensions_worktree_config_is_refused_only_when_git_says_it_is_on() {
    // `--type=bool` is why this is one rule and not three spellings: git's own words for "on" all answer
    // `true`, and an explicit `false` is a repository that may be mounted.
    let dir = TempDir::new("refusal-worktree-config");
    let (_, repo) = projects(&dir);
    for (value, expected) in [
        ("true", Some(Refusal::WorktreeConfigEnabled)),
        ("on", Some(Refusal::WorktreeConfigEnabled)),
        ("false", None),
        ("off", None),
    ] {
        support::git(&repo, &["config", "extensions.worktreeConfig", value]);
        assert_eq!(
            refused(&repo),
            expected,
            "extensions.worktreeConfig = {value}"
        );
    }
}

#[test]
fn a_repository_git_cannot_read_is_not_treated_as_mountable() {
    // Not a policy but the absence of an answer: git refuses a boolean key it cannot parse, so nothing
    // about this repository can be promised. The create answers `500 internal` and logs git's words —
    // the assertion here is that the fourth variant carries them rather than claiming the project is fine.
    let dir = TempDir::new("refusal-unreadable");
    let (_, repo) = projects(&dir);
    let config = std::fs::read_to_string(repo.join(".git/config")).expect("the config reads");
    std::fs::write(
        repo.join(".git/config"),
        format!("{config}[extensions]\n\tworktreeConfig = maybe\n"),
    )
    .expect("the config is writable");

    match refused(&repo) {
        Some(Refusal::Unreadable(error)) => assert!(
            error.detail.contains("bad boolean config value"),
            "git's own words are not carried: {error}"
        ),
        other => panic!("a repository git cannot read answered {other:?}"),
    }
}

#[tokio::test]
async fn a_refused_create_answers_422_with_its_own_code_and_writes_nothing() {
    // The story's demo, one project per hole: three codes, each 422 in ADR-0008's envelope, and no
    // session, no worktree directory and no registration left behind — the fourth acceptance line.
    let dir = TempDir::new("refusal-api");
    let root = dir.file("projects");

    let linked = root.join("linked");
    repository(&linked);
    std::fs::rename(linked.join(".git"), linked.join("moved.git"))
        .expect("the repository's .git is movable");
    std::fs::write(linked.join(".git"), "gitdir: moved.git\n").expect("a .git file is writable");

    let submodule = root.join("submodule");
    repository(&submodule);
    std::fs::write(submodule.join(".gitmodules"), GITMODULES).expect(".gitmodules is writable");

    let configured = root.join("configured");
    repository(&configured);
    support::git(
        &configured,
        &["config", "extensions.worktreeConfig", "true"],
    );

    let state = docker_state(&dir, &root);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    for (project, code) in [
        ("linked", "git_not_directory"),
        ("submodule", "submodules_present"),
        ("configured", "worktree_config_enabled"),
    ] {
        let response = post_create_for(served.addr, project, &format!("refusal-{project}")).await;
        assert_eq!(response.status, 422, "{project}: {}", response.body);
        assert_eq!(
            response.header("content-type"),
            Some("application/json"),
            "{project}: {}",
            response.body
        );
        let body: Value = serde_json::from_str(&response.body).expect("an envelope is JSON");
        assert_eq!(body["code"], code, "{project}: {}", response.body);
        let sentence = body["message"]
            .as_str()
            .expect("the envelope has a sentence");
        assert!(
            !sentence.contains(code),
            "the sentence repeats the code instead of explaining it: {sentence}"
        );
        assert!(
            sentence.contains("refused"),
            "the sentence does not say the project was refused: {sentence}"
        );
    }

    // "so a refused session leaves nothing behind": no row, no worktree directory, and every project
    // still holds exactly its own checkout.
    {
        let database = state.database.lock().expect("the lock is not poisoned");
        assert!(
            database
                .sessions()
                .list(None)
                .expect("the list reads")
                .is_empty(),
            "a refused create wrote a session row"
        );
    }
    assert!(
        !root.join(".worktrees").exists(),
        "a refused create made a worktree directory"
    );
    for project in ["linked", "submodule", "configured"] {
        let repo = root.join(project);
        // One entry: the repository's own checkout. A session's would be a second, and the id it is named
        // after never existed — so the count is the whole claim, not the path (a `.git` file's repository
        // reports its gitdir's path, which is the fixture's second name).
        assert_eq!(
            support::worktrees(&repo).len(),
            1,
            "{project} holds a worktree registration a refused create made"
        );
    }
}

#[tokio::test]
async fn a_refused_create_does_not_evict_anyone() {
    // The refusal is answered before the cap, so a full machine does not stop an idle session to make
    // room for work that will not happen. The refused create's code is the project's, not `cap_reached`,
    // and the live session is untouched.
    let dir = TempDir::new("refusal-before-cap");
    let root = dir.file("projects");
    let repo = root.join("holosmith");
    repository(&repo);
    std::fs::write(repo.join(".gitmodules"), GITMODULES).expect(".gitmodules is writable");

    let config = Config {
        projects_root: root.clone(),
        max_live_sessions: 1,
        ..Config::default()
    };
    let mut state = state_in(&dir, config);
    state.backend = Backends::for_run(Backend::Docker, settings(&root));
    let live = create_row(&state);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let response = post_create(served.addr, "refusal-at-the-cap").await;
    assert_eq!(response.status, 422, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("an envelope is JSON");
    assert_eq!(
        body["code"], "submodules_present",
        "a refused create answered the cap instead of the project: {}",
        response.body
    );
    assert_eq!(
        row(&state, &live.id).status,
        Status::Starting,
        "a refused create moved a session on its way to saying no"
    );
}

#[tokio::test]
async fn a_retry_is_answered_from_its_row_even_after_the_project_became_unsafe() {
    // ADR-0012's promise is that a retry creates nothing and is answered from the row it already has.
    // The repository is read on every create (a project can change under the server), but the refusal is
    // only an *answer* where a row would be written — so a retry after the project was made unsafe
    // still gets the session it asked for.
    let dir = TempDir::new("refusal-retry");
    let root = dir.file("projects");
    let repo = root.join("holosmith");
    repository(&repo);
    let state = docker_state(&dir, &root);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let id = created_id(&post_create(served.addr, "refusal-retry-1").await);
    std::fs::write(repo.join(".gitmodules"), GITMODULES).expect(".gitmodules is writable");

    let again = post_create(served.addr, "refusal-retry-1").await;
    assert_eq!(again.status, 202, "{}", again.body);
    assert_eq!(created_id(&again), id, "the retry created a second session");
}

#[tokio::test]
async fn a_stub_create_has_no_repository_to_refuse() {
    // The refusal is the backend's because the *repository* is the backend's: a stub session provisions
    // nothing outside the process, so a machine with no projects root at all still creates sessions —
    // which is what every stub test and the gate's own boot look like (ADR-0015).
    let dir = TempDir::new("refusal-stub");
    let state = state_in(&dir, Config::default());
    assert_eq!(
        state.runtime.backend,
        Backend::Stub,
        "this test is the stub's"
    );
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let response = post_create(served.addr, "refusal-stub-1").await;
    assert_eq!(response.status, 202, "{}", response.body);
}

#[tokio::test]
async fn a_resume_refuses_a_project_the_create_would_have_refused() {
    // The refusal is the *machine's* answer about the project, not the create's convenience: a session
    // created while the project was plain is not brought back into a mount the same create would refuse
    // today ("so that I do not discover the hole after an agent exploits it"). The row is untouched, so
    // the operator can fix the project and resume again.
    let dir = TempDir::new("refusal-resume");
    let root = dir.file("projects");
    let repo = root.join("holosmith");
    repository(&repo);
    let state = docker_state(&dir, &root);
    let stopped = create_row(&state);
    let transitioner = state.transitioner();
    transitioner
        .apply(&stopped.id, Trigger::PiReady, None)
        .await
        .expect("starting -> idle is the table's");
    transitioner
        .apply(&stopped.id, Trigger::IdleStop, Some("idle_stop"))
        .await
        .expect("idle -> stopped is the table's");
    std::fs::write(repo.join(".gitmodules"), GITMODULES).expect(".gitmodules is writable");

    let served = Served::start(http::router(state.clone()), GRACE).await;
    let text = format!(
        "POST /api/sessions/{}/resume HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n",
        stopped.id
    );
    let response = request(served.addr, &text).await;

    assert_eq!(response.status, 422, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("an envelope is JSON");
    assert_eq!(body["code"], "submodules_present", "{}", response.body);
    assert_eq!(
        row(&state, &stopped.id).status,
        Status::Stopped,
        "a refused resume moved the row"
    );
}

#[tokio::test]
async fn a_repository_git_cannot_read_answers_500_with_gits_words_in_the_log_only() {
    // `Refusal::Unreadable` is not a refusal the client can act on, so it is not a 422: git's message names
    // a config file on the machine, and a phone cannot fix it. ADR-0008's `internal` sentence is the answer,
    // and the log is where the words go — the assertion is that they are *not* in the body.
    let dir = TempDir::new("refusal-unreadable-api");
    let root = dir.file("projects");
    let repo = root.join("holosmith");
    repository(&repo);
    let config = std::fs::read_to_string(repo.join(".git/config")).expect("the config reads");
    std::fs::write(
        repo.join(".git/config"),
        format!("{config}[extensions]\n\tworktreeConfig = maybe\n"),
    )
    .expect("the config is writable");
    let state = docker_state(&dir, &root);
    let served = Served::start(http::router(state.clone()), GRACE).await;

    let response = post_create(served.addr, "refusal-unreadable").await;

    assert_eq!(response.status, 500, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let body: Value = serde_json::from_str(&response.body).expect("an envelope is JSON");
    assert_eq!(body["code"], "internal", "{}", response.body);
    assert!(
        !response.body.contains("boolean"),
        "git's own words leaked to the client: {}",
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
}
