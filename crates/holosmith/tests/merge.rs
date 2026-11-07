//! The merge's fast-forward path over a real repository (HS-054, spec item 12's rules 2 and 4).
//!
//! Every acceptance bullet of the story is a *git* fact — a ref that moved or did not, a working tree that
//! changed or did not, a hook that ran or did not — so these tests own a real repository in a `TempDir`, exactly
//! as `tests/changes.rs` does for the read side, and move refs and files between two assertions. No Docker, no
//! daemon, no backend: item 12's sentence is that all merge work runs on the host in the project repository, so
//! the interesting half needs none of them.
//!
//! The wire group at the end boots the real router over the same repository, because a `TempDir` cannot prove
//! what a client sees: the three keys, the `strategy` default, and the six refusals as status **and** code.
//!
//! Two of the story's decisions are only provable by measuring the thing that would otherwise happen:
//!
//! - **the hook test** writes a real `post-merge` hook and asserts its marker never appears, which is the only
//!   way to show that `-c core.hooksPath=/dev/null` is load-bearing rather than decorative;
//! - **the unrelated-dirty-file test** asserts that a checkout with work in it still merges, which is what makes
//!   "measure the failure rather than pre-check the status" a decision with a test behind it instead of a taste.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::Method;
use holosmith::changes::ChangesError;
use holosmith::clock;
use holosmith::config::Config;
use holosmith::http::{self, AppState};
use holosmith::merge::{FF_ONLY, MergeError, Merged, merge};
use holosmith::sessions::NewSession;
use holosmith::worktree::Worktree;
use serde_json::Value;

mod support;
use support::{Served, TempDir, fetch, git, head, repository_on, request, state_in};

/// The drain deadline each served test gives `serve`.
const GRACE: Duration = Duration::from_millis(100);

/// `Config::default().control_origin`, which is what the default guards expect.
const ORIGIN: &str = "http://localhost:8080";

/// One session's repository: the data root, the projects root, the repository, the worktree and the row.
///
/// Built the way the provisioner builds it — `git worktree add -b agent/<id> <path> <base>` and one
/// `set_worktree` — and then driven directly, which is what lets a test compare a ref against `git rev-parse`
/// it runs itself rather than against the value the code just answered.
struct Fixture {
    // Held for the lifetime of the fixture: the database, the repository and the worktree live under it.
    _dir: TempDir,
    state: AppState,
    projects: PathBuf,
    repo: PathBuf,
    worktree: PathBuf,
    id: String,
    base: String,
}

impl Fixture {
    /// A repository on `main` with one commit, a session row and its worktree.
    fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        let projects = dir.file("projects");
        let repo = projects.join("holosmith");
        repository_on(&repo, "main");
        let base = head(&repo);
        let config = Config {
            data_root: dir.path().to_path_buf(),
            projects_root: projects.clone(),
            ..Config::default()
        };
        let state = state_in(&dir, config);
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
                        client_key: Some(format!("merge-{label}")),
                        base_commit: None,
                    },
                    clock::unix_now(),
                )
                .expect("a session is created")
                .id
        };
        let worktree = projects.join(".worktrees").join(&id);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &format!("agent/{id}"),
                worktree.to_str().expect("a UTF-8 temp path"),
                &base,
            ],
        );
        {
            let database = state.database.lock().expect("the lock is not poisoned");
            database
                .sessions()
                .set_worktree(&id, worktree.to_str().expect("a UTF-8 temp path"), &base)
                .expect("the row records its worktree");
        }
        Self {
            _dir: dir,
            state,
            projects,
            repo,
            worktree,
            id,
            base,
        }
    }

    fn branch(&self) -> String {
        format!("agent/{}", self.id)
    }

    fn worktree(&self) -> Worktree {
        Worktree::of(&self.projects, "holosmith", &self.id)
    }

    /// The answer `POST …/merge` would give, with the machine's default targets.
    fn run(&self, target: &str) -> Result<Merged, MergeError> {
        self.run_with(target, &["main".to_string(), "master".to_string()])
    }

    fn run_with(&self, target: &str, targets: &[String]) -> Result<Merged, MergeError> {
        merge(
            &self.worktree(),
            target,
            targets,
            &self.state.merges.clone(),
        )
    }

    fn refused(&self, target: &str) -> MergeError {
        self.run(target).expect_err("the merge is refused")
    }

    /// Write `path` in the worktree and commit it on the session branch.
    fn commit(&self, path: &str, contents: &[u8], message: &str) {
        write(&self.worktree, path, contents);
        git(&self.worktree, &["add", path]);
        git(&self.worktree, &["commit", "--quiet", "-m", message]);
    }

    /// Write `path` in the project's own checkout and commit it on `main` — the target moving on.
    fn commit_target(&self, path: &str, contents: &[u8], message: &str) {
        write(&self.repo, path, contents);
        git(&self.repo, &["add", path]);
        git(&self.repo, &["commit", "--quiet", "-m", message]);
    }

    /// The commit a ref points at, read by the test rather than by the code under test.
    fn rev(&self, reference: &str) -> String {
        git(&self.repo, &["rev-parse", reference])
    }

    fn status(&self, dir: &Path) -> String {
        git(dir, &["status", "--porcelain"])
    }

    async fn served(&self) -> Served {
        Served::start(http::router(self.state.clone()), GRACE).await
    }
}

/// Write a file, making its parent directory when it has one.
fn write(directory: &Path, path: &str, contents: &[u8]) {
    let full = directory.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("the directory is creatable");
    }
    std::fs::write(&full, contents).expect("the file is written");
}

// --- the two update paths ---------------------------------------------------------------------------------

#[test]
fn a_checked_out_target_fast_forwards_and_its_working_tree_moves() {
    // Acceptance bullet three, in the ordinary shape: `main` is checked out in the project's own checkout, so
    // the merge runs *there* and the operator's files move with the ref. Two commits on the branch, so
    // "the tip" is not the same as "one commit after the base".
    let fixture = Fixture::new("merge-checked-out");
    fixture.commit("a.txt", b"a\n", "one file");
    fixture.commit("c.txt", b"c\n", "a second file");
    let tip = fixture.rev(&fixture.branch());
    assert_ne!(tip, fixture.base);

    let merged = fixture.run("main").expect("the fast-forward lands");

    assert_eq!(merged.merged_commit, tip);
    assert_eq!(merged.target, "main");
    assert_eq!(merged.strategy, FF_ONLY);
    // The ref moved...
    assert_eq!(fixture.rev("refs/heads/main"), tip);
    // ... and so did the operator's working tree: the agent's files are on disk in the project's checkout, and
    // the checkout is clean afterwards (a fast-forward that only moved the ref would leave them staged-as-new).
    assert_eq!(
        std::fs::read(fixture.repo.join("a.txt")).expect("a.txt is in the checkout"),
        b"a\n"
    );
    assert_eq!(
        std::fs::read(fixture.repo.join("c.txt")).expect("c.txt is in the checkout"),
        b"c\n"
    );
    assert_eq!(fixture.status(&fixture.repo), "");
}

#[test]
fn an_unchecked_out_target_moves_by_compare_and_swap_and_its_checkout_does_not() {
    // Acceptance bullet four, measured apart from bullet three because the two paths differ in *what moves*.
    // Detaching the project's checkout is what makes the target unchecked-out: `main` is then a ref no working
    // tree holds, so rule 4's first arm is the one that runs.
    let fixture = Fixture::new("merge-unchecked-out");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    git(
        &fixture.repo,
        &["checkout", "--quiet", "--detach", &fixture.base],
    );

    let merged = fixture.run("main").expect("the compare-and-swap lands");

    assert_eq!(merged.merged_commit, tip);
    // The ref moved ...
    assert_eq!(fixture.rev("refs/heads/main"), tip);
    // ... and the checkout did **not**: it is detached at the base, so the agent's file is not there. This is
    // the assertion that tells the two update paths apart — an implementation that always ran `merge --ff-only`
    // somewhere would have moved a working tree, and one that only moved the ref regardless of the checkout
    // would have left `c.txt` out of a checkout that was supposed to receive it.
    assert!(
        !fixture.repo.join("a.txt").exists(),
        "the detached checkout received the merge's files"
    );
    assert_eq!(head(&fixture.repo), fixture.base);
}

#[test]
fn a_target_ahead_of_the_branch_is_not_fast_forward_and_nothing_moves() {
    // Acceptance bullet three's second half: the target has a commit the branch lacks, so there is nothing to
    // fast-forward *to*. Both refs and both working trees are unchanged, which is what "nothing was merged"
    // has to mean.
    let fixture = Fixture::new("merge-behind");
    fixture.commit("a.txt", b"a\n", "the session's change");
    fixture.commit_target("t.txt", b"target\n", "the target moves on");
    let before = (
        fixture.rev("refs/heads/main"),
        fixture.rev(&fixture.branch()),
        fixture.status(&fixture.repo),
    );

    match fixture.refused("main") {
        MergeError::NotFastForward { target, branch } => {
            assert_eq!(target, "main");
            assert_eq!(branch, fixture.branch());
        }
        other => panic!("a target ahead of the branch is not a refusal: {other:?}"),
    }

    assert_eq!(
        (
            fixture.rev("refs/heads/main"),
            fixture.rev(&fixture.branch()),
            fixture.status(&fixture.repo)
        ),
        before,
        "a refused merge moved something"
    );
}

#[test]
fn a_dirty_checkout_that_would_be_overwritten_is_target_dirty() {
    // Acceptance bullet five. The branch *adds* `a.txt`; the checkout has an untracked `a.txt` of its own, so
    // git refuses to overwrite it. Nothing moves and the operator's file is untouched — a merge that clobbered
    // it would be worse than the refusal.
    let fixture = Fixture::new("merge-dirty");
    fixture.commit("a.txt", b"the session's version\n", "the session's change");
    let main = fixture.rev("refs/heads/main");
    write(&fixture.repo, "a.txt", b"the operator's work in progress\n");

    match fixture.refused("main") {
        MergeError::TargetDirty { target, path } => {
            assert_eq!(target, "main");
            assert_eq!(path, fixture.repo);
        }
        other => panic!("a checkout that would be overwritten is not a refusal: {other:?}"),
    }

    assert_eq!(fixture.rev("refs/heads/main"), main, "the ref moved");
    assert_eq!(
        std::fs::read(fixture.repo.join("a.txt")).expect("the file is still there"),
        b"the operator's work in progress\n",
        "the refusal overwrote the operator's file"
    );
}

#[test]
fn an_unrelated_dirty_file_does_not_block_the_merge() {
    // The test that makes "run the merge, classify the failure" a decision rather than a taste: a *pre-check* of
    // the checkout's status would refuse this merge, because the checkout holds work — but the work is a file
    // the fast-forward does not touch, and git carries it across happily. Refusing here would be a merge the
    // operator cannot perform for a reason that is not true.
    let fixture = Fixture::new("merge-unrelated-dirty");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    write(&fixture.repo, "scratch.txt", b"mine\n");

    let merged = fixture
        .run("main")
        .expect("an unrelated dirty file blocks nothing");

    assert_eq!(merged.merged_commit, tip);
    assert_eq!(fixture.rev("refs/heads/main"), tip);
    assert_eq!(
        std::fs::read(fixture.repo.join("scratch.txt")).expect("the file survives"),
        b"mine\n"
    );
    assert_eq!(
        fixture.status(&fixture.repo),
        "?? scratch.txt",
        "the merge disturbed the operator's own work"
    );
}

// --- the target's three facts -----------------------------------------------------------------------------

#[test]
fn a_target_the_configuration_does_not_list_is_refused_before_git_is_asked() {
    // The first of the two `422` facts: the request named a branch this machine will not merge into. The list is
    // the config's, so the answer does not depend on the repository at all — `trunk` exists here and is still
    // refused, which is what makes this assertion about the *list* rather than about the branch.
    let fixture = Fixture::new("merge-unlisted");
    fixture.commit("a.txt", b"a\n", "the session's change");
    git(&fixture.repo, &["branch", "trunk"]);

    match fixture.run("trunk") {
        Err(MergeError::Unlisted { target, targets }) => {
            assert_eq!(target, "trunk");
            assert_eq!(targets, vec!["main".to_string(), "master".to_string()]);
        }
        other => panic!("an unlisted target is not refused: {other:?}"),
    }
    assert_eq!(fixture.rev("refs/heads/trunk"), fixture.base, "trunk moved");
}

#[test]
fn a_listed_target_the_repository_does_not_have_is_refused() {
    // The second of the two: the default list names both `main` and `master` because a machine has one of them,
    // so a listed name the project does not have is the ordinary case and not a mistake. It is still the
    // parameter's fault — a `422` — and ADR-0061's "a name the repository does not have is not a target" is the
    // same rule one route over.
    let fixture = Fixture::new("merge-no-such-branch");
    fixture.commit("a.txt", b"a\n", "the session's change");

    match fixture.refused("master") {
        MergeError::NoTarget { target } => assert_eq!(target, "master"),
        other => panic!("a missing branch is not refused: {other:?}"),
    }
    assert!(!support::branch_exists(&fixture.repo, "master"));
}

#[test]
fn a_target_checked_out_in_another_worktree_is_refused() {
    // Rule 4's third answer. `main` is checked out in a worktree that is not the project's own checkout — which
    // is what a session's worktree looks like after an agent runs `git checkout main` — so the merge refuses
    // rather than moving files under whoever is working there. The alternative (updating *that* working tree) is
    // the one thing this build must not do to a running container.
    let fixture = Fixture::new("merge-checked-out-elsewhere");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    // The project's own checkout leaves `main` so a second worktree can hold it.
    git(&fixture.repo, &["checkout", "--quiet", "-b", "elsewhere"]);
    let other = fixture.projects.join("another-checkout");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            other.to_str().expect("a UTF-8 temp path"),
            "main",
        ],
    );

    match fixture.refused("main") {
        MergeError::CheckedOutElsewhere { target, path } => {
            assert_eq!(target, "main");
            assert_eq!(path, other);
        }
        other => panic!("a checkout the merge must not touch is not refused: {other:?}"),
    }
    assert_ne!(
        fixture.rev("refs/heads/main"),
        tip,
        "the refusal moved the target anyway"
    );
    assert!(
        !other.join("a.txt").exists(),
        "the refusal wrote into a working tree it said it would not touch"
    );
}

// --- the facts around a merge that does happen ------------------------------------------------------------

#[test]
fn a_second_merge_of_the_same_session_is_a_no_op_that_names_the_same_commit() {
    // The story's answer to "what does a second merge do", and it is measured rather than special-cased: after
    // the first merge the target **is** the branch's tip, so the target is still an ancestor of the branch and
    // `merge --ff-only` answers "Already up to date". A merge that invented a second commit, or a refusal for a
    // request that is already satisfied, would both be wrong.
    let fixture = Fixture::new("merge-twice");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let first = fixture.run("main").expect("the first merge lands");
    let status = fixture.status(&fixture.repo);
    let reflog = git(&fixture.repo, &["reflog", "show", "refs/heads/main"]);

    let second = fixture.run("main").expect("a second merge is not an error");

    assert_eq!(second, first);
    assert_eq!(fixture.rev("refs/heads/main"), first.merged_commit);
    assert_eq!(
        fixture.status(&fixture.repo),
        status,
        "the tree moved again"
    );
    assert_eq!(
        git(&fixture.repo, &["reflog", "show", "refs/heads/main"]),
        reflog,
        "the no-op appended a reflog entry, so the ref was written"
    );
}

#[test]
fn a_hook_does_not_run() {
    // Rule 2's `-c core.hooksPath=/dev/null`, measured. A `post-merge` hook is the one hook a fast-forward can
    // run, and this build will not run a project's program on an agent's behalf. Dropping the flag makes this
    // test red by creating the marker — which is the only proof that the flag is load-bearing.
    let fixture = Fixture::new("merge-hooks");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let marker = fixture._dir.file("hook-ran");
    let hook = fixture.repo.join(".git").join("hooks").join("post-merge");
    std::fs::create_dir_all(hook.parent().expect("the hooks directory has a parent"))
        .expect("the hooks directory is creatable");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\n",
            marker.to_str().expect("a UTF-8 temp path")
        ),
    )
    .expect("the hook is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("the hook is executable");
    }

    fixture.run("main").expect("the merge lands");

    assert!(
        !marker.exists(),
        "a project's post-merge hook ran: agent-written code was executed on the host"
    );
}

#[test]
fn a_worktree_that_is_gone_is_the_changes_view_s_refusal() {
    // The merge inherits the changes view's ordered refusals rather than spelling its own: "there is no worktree
    // to merge" is that module's fact and its sentence, so a client that read `GET …/changes` learns nothing new
    // here. A `500` from a git asked about a directory that is not there would be the wrong family entirely.
    let fixture = Fixture::new("merge-gone");
    std::fs::remove_dir_all(&fixture.worktree).expect("the worktree is removed");

    match fixture.refused("main") {
        MergeError::Changes(ChangesError::Missing { .. }) => {}
        other => panic!("a missing worktree is not the changes view's refusal: {other:?}"),
    }
}

// --- the merge at the wire ---------------------------------------------------------------------------------

/// `POST /api/sessions/{id}/merge` with a body, the way the app's own client sends it.
async fn post_merge(addr: std::net::SocketAddr, id: &str, body: &str) -> support::Response {
    request(
        addr,
        &format!(
            "POST /api/sessions/{id}/merge HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
             Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        ),
    )
    .await
}

fn envelope(response: &support::Response) -> Value {
    serde_json::from_str(&response.body).expect("the envelope is JSON")
}

#[tokio::test]
async fn the_endpoint_answers_the_three_keys_and_the_target_advances() {
    // The demo as an assertion, over the real router: `{target:"main"}` with no `strategy` — so the response's
    // `strategy` is the default rather than an echo — and the commit it names is the commit the target is at,
    // read by the test from the repository.
    let fixture = Fixture::new("merge-wire");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    let served = fixture.served().await;
    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    assert_eq!(body["merged_commit"], tip);
    assert_eq!(body["target"], "main");
    assert_eq!(
        body["strategy"], FF_ONLY,
        "the default strategy is not echoed"
    );
    assert_eq!(body.as_object().expect("an object").len(), 3);
    assert_eq!(fixture.rev("refs/heads/main"), tip);
    assert_eq!(
        std::fs::read(fixture.repo.join("a.txt")).expect("the file is in the checkout"),
        b"a\n"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn an_explicit_ff_only_is_the_same_request_as_no_strategy() {
    let fixture = Fixture::new("merge-wire-ff");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let served = fixture.served().await;
    let response = post_merge(
        served.addr,
        &fixture.id,
        r#"{"target":"main","strategy":"ff-only","message":"ignored"}"#,
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    assert_eq!(body["strategy"], FF_ONLY);
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn the_four_bad_requests_move_no_ref() {
    // The acceptance's first two bullets and the two `unsupported`s, at the wire: the codes are the machine's and
    // the refs are the repository's, and neither a refused strategy nor a bad target moves anything.
    let fixture = Fixture::new("merge-wire-refusals");
    fixture.commit("a.txt", b"a\n", "the session's change");
    git(&fixture.repo, &["branch", "trunk"]);
    let before = (
        fixture.rev("refs/heads/main"),
        fixture.rev("refs/heads/trunk"),
        fixture.rev(&fixture.branch()),
    );
    let served = fixture.served().await;
    for (body, code) in [
        (r#"{"target":"nope"}"#, "bad_target"),
        (r#"{"target":"trunk"}"#, "bad_target"),
        (r#"{"target":"main","strategy":"squash"}"#, "unsupported"),
        (r#"{"target":"main","delete_session":true}"#, "unsupported"),
        (r#"{"target":"main","strategy":"rebase"}"#, "bad_strategy"),
        (r#"{"target":"main","force":true}"#, "unknown_field"),
        (r#"{"strategy":"ff-only"}"#, "bad_body"),
    ] {
        let response = post_merge(served.addr, &fixture.id, body).await;
        assert_eq!(response.status, 422, "{body}: {}", response.body);
        assert_eq!(envelope(&response)["code"], code, "{body}");
        assert_eq!(response.header("content-type"), Some("application/json"));
    }
    assert_eq!(
        (
            fixture.rev("refs/heads/main"),
            fixture.rev("refs/heads/trunk"),
            fixture.rev(&fixture.branch())
        ),
        before,
        "a refused request moved a ref"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn the_two_conflicts_are_409s_with_their_own_codes() {
    // `not_fast_forward` and `target_dirty` as a client sees them: the spec's own words, a `409`, and a sentence
    // that names the next step. Two fixtures because the two facts cannot both hold for one repository.
    let behind = Fixture::new("merge-wire-behind");
    behind.commit("a.txt", b"a\n", "the session's change");
    behind.commit_target("t.txt", b"target\n", "the target moves on");
    let served = behind.served().await;
    let response = post_merge(served.addr, &behind.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 409, "{}", response.body);
    let body = envelope(&response);
    assert_eq!(body["code"], "not_fast_forward");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("nothing was merged")),
        "the sentence does not say nothing happened: {body}"
    );
    served.finish().await.expect("the server stops");

    let dirty = Fixture::new("merge-wire-dirty");
    dirty.commit("a.txt", b"the session's version\n", "the session's change");
    write(&dirty.repo, "a.txt", b"the operator's work\n");
    let served = dirty.served().await;
    let response = post_merge(served.addr, &dirty.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "target_dirty");
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_session_with_no_worktree_is_the_changes_view_s_409_here_too() {
    // The handler's half of the shared refusal: a row with no `base_commit` has no branch to merge, and it is
    // answered before git is asked anything — with the same code and the same sentence `GET …/changes` uses.
    let fixture = Fixture::new("merge-wire-no-worktree");
    let fresh = {
        let database = fixture
            .state
            .database
            .lock()
            .expect("the lock is not poisoned");
        database
            .sessions()
            .create(
                &NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: Some("merge-no-worktree".to_string()),
                    base_commit: None,
                },
                clock::unix_now(),
            )
            .expect("a second session is created")
            .id
    };
    let served = fixture.served().await;
    let response = post_merge(served.addr, &fresh, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "no_worktree");
    // ... and an id the table does not hold is the envelope's `404`, not this route's problem.
    let unknown = post_merge(served.addr, "0123456789abcdef", r#"{"target":"main"}"#).await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    assert_eq!(envelope(&unknown)["code"], "not_found");
    served.finish().await.expect("the server stops");
}

#[test]
fn the_merge_route_is_mutating_and_in_the_manifest() {
    // A `POST`, so the manifest records `Mutating` and every guard applies — the property that keeps a route that
    // writes a repository behind ADR-0010's three headers. The kind is asked of the app rather than assumed.
    let fixture = Fixture::new("merge-guards");
    let app = http::App::new(
        http::router(fixture.state.clone()),
        http::Guards::from_config(&fixture.state.config),
    );
    let entry = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}/merge")
        .expect("the route is in the manifest");
    assert_eq!(entry.kind, http::RouteKind::Mutating);
    assert_eq!(entry.method, Method::POST);
}

#[tokio::test]
async fn a_merge_without_the_client_headers_never_reaches_the_repository() {
    // ADR-0010's guards, applied to the first route that writes a repository: a request that is not the
    // Holosmith client is refused before the handler, so the target cannot have moved.
    let fixture = Fixture::new("merge-wire-guards");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let main = fixture.rev("refs/heads/main");
    let served = fixture.served().await;
    let body = r#"{"target":"main"}"#;
    let response = request(
        served.addr,
        &format!(
            "POST /api/sessions/{}/merge HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            fixture.id,
            body.len()
        ),
    )
    .await;
    assert_eq!(response.status, 403, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "bad_client");
    assert_eq!(fixture.rev("refs/heads/main"), main, "the target moved");
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn the_sessions_changes_view_answers_ahead_zero_after_a_merge() {
    // Nothing about the row changes, and the read that describes the branch says so: `ahead` is a measurement of
    // `base_commit..agent/<id>`, and the merge moved the *target* rather than the branch, so what changes is
    // `behind` (to 0) and `mergeable` (still true, of a no-op). Asserted here because it is the sentence ADR-0063
    // records as the answer to "what does a merge tell the changes view".
    let fixture = Fixture::new("merge-wire-changes");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let served = fixture.served().await;
    let before = fetch(
        served.addr,
        &format!("/api/sessions/{}/changes", fixture.id),
    )
    .await;
    assert_eq!(before.status, 200, "{}", before.body);
    let before: Value = serde_json::from_str(&before.body).expect("the summary is JSON");
    assert_eq!(before["ahead"], 1);
    assert_eq!(before["behind"], 0);

    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 200, "{}", response.body);

    let after = fetch(
        served.addr,
        &format!("/api/sessions/{}/changes", fixture.id),
    )
    .await;
    assert_eq!(after.status, 200, "{}", after.body);
    let after: Value = serde_json::from_str(&after.body).expect("the summary is JSON");
    // The branch did not move ...
    assert_eq!(after["ahead"], 1);
    assert_eq!(after["base_commit"], before["base_commit"]);
    // ... and the target now holds everything the branch does.
    assert_eq!(after["behind"], 0);
    assert_eq!(after["mergeable"], true);
    served.finish().await.expect("the server stops");
}
