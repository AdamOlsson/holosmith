//! The merge over a real repository: the fast-forward and the two strategies that build a commit
//! spec item 12's rules 2, 3 and 4.
//!
//! Every acceptance bullet of both stories is a *git* fact — a ref that moved or did not, a working tree that
//! changed or did not, a hook that ran or did not, a commit whose parents are these two and not those — so these
//! tests own a real repository in a `TempDir`, exactly as `tests/changes.rs` does for the read side, and move
//! refs and files between two assertions. No Docker, no daemon, no backend: item 12's sentence is that all merge
//! work runs on the host in the project repository, so the interesting half needs none of them.
//!
//! The wire group at the end boots the real router over the same repository, because a `TempDir` cannot prove
//! what a client sees: the three keys, the `strategy` default and echo, and every refusal as status **and** code.
//!
//! Four of the decisions are only provable by measuring the thing that would otherwise happen:
//!
//! - **the hook tests** write real `post-merge`, `post-checkout`, `pre-commit` and `commit-msg` hooks and assert
//!   their markers never appear, which is the only way to show that `-c core.hooksPath=/dev/null` is load-bearing
//!   rather than decorative;
//! - **the unrelated-dirty-file test** asserts that a checkout with work in it still merges, which is what makes
//!   "measure the failure rather than pre-check the status" a decision with a test behind it instead of a taste;
//! - **the temporary-directory assertions** read the filesystem (`<data>/merge` and the registration list) after
//!   a success, a conflict and a refusal that happened *after* the landing — "removed afterwards even on failure"
//!   is a fact about a directory, not about a return value;
//! - **the keep-rule test** asks the function after each strategy, because a squash leaves the branch a
//!   non-ancestor of the target and that has a consequence for a delete nobody has built yet.
//!
//! The unrelated [identity] claim is measured the only way it can be on a machine that has a global `user.name`:
//! by reading `%an <%ae>` off the commit this build wrote.
//!
//! [identity]: holosmith::snapshot::IDENTITY

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::http::Method;
use holosmith::changes::ChangesError;
use holosmith::clock;
use holosmith::config::Config;
use holosmith::http::{self, AppState};
use holosmith::merge::{FF_ONLY, MergeError, Merged, Request, Strategy, merge};
use holosmith::sessions::NewSession;
use holosmith::sessions::Status;
use holosmith::sessions::state::{Transitioner, Trigger};
use holosmith::worktree::{Retained, Worktree};
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

    /// The answer `POST …/merge` would give, with the machine's default targets and the default strategy.
    fn run(&self, target: &str) -> Result<Merged, MergeError> {
        self.run_with(target, &["main".to_string(), "master".to_string()])
    }

    fn run_with(&self, target: &str, targets: &[String]) -> Result<Merged, MergeError> {
        self.land(
            &Request {
                target: target.to_string(),
                strategy: Strategy::FastForward,
                message: None,
            },
            targets,
        )
    }

    /// One request, with the strategy under test.
    fn land(&self, request: &Request, targets: &[String]) -> Result<Merged, MergeError> {
        merge(
            &self.worktree(),
            request,
            targets,
            &self.state.merges.clone(),
            &self.state.config.data_root,
        )
    }

    /// A `merge` or a `squash` request into `main`, with an optional commit message.
    fn land_by(&self, strategy: Strategy, message: Option<&str>) -> Result<Merged, MergeError> {
        self.land(
            &Request {
                target: "main".to_string(),
                strategy,
                message: message.map(str::to_string),
            },
            &["main".to_string(), "master".to_string()],
        )
    }

    fn refused(&self, target: &str) -> MergeError {
        self.run(target).expect_err("the merge is refused")
    }

    /// The paths under `<data>/merge`, as the operator would see them: the measure of "a temporary worktree
    /// was removed", which is a fact about the filesystem and not about what the code says it did.
    fn temporary(&self) -> Vec<String> {
        let directory = self.state.config.data_root.join("merge");
        let mut found: Vec<String> = match std::fs::read_dir(&directory) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect(),
            Err(_) => Vec::new(),
        };
        found.sort();
        found
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

    /// The commits a ref names as its parents, from `git rev-list --parents -n 1`.
    fn parents(&self, reference: &str) -> Vec<String> {
        let line = git(&self.repo, &["rev-list", "--parents", "-n", "1", reference]);
        line.split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect()
    }

    /// The tree a commit holds, which is the assertion a `git log --oneline` cannot make: a squash's history
    /// is one commit long whatever its content, so the content is what has to be compared.
    fn tree(&self, reference: &str) -> String {
        git(&self.repo, &["rev-parse", &format!("{reference}^{{tree}}")])
    }

    /// A second session's worktree on the **same** repository, for the concurrency test: rule 2's "one merge
    /// per project" is about one repository, so two sessions of one project are the pair that has to serialise.
    fn other_session(&self, label: &str) -> (String, PathBuf) {
        let id = {
            let database = self
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
                        client_key: Some(format!("merge-{label}")),
                        base_commit: None,
                    },
                    clock::unix_now(),
                )
                .expect("a second session is created")
                .id
        };
        let path = self.projects.join(".worktrees").join(&id);
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &format!("agent/{id}"),
                path.to_str().expect("a UTF-8 temp path"),
                &self.base,
            ],
        );
        (id, path)
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

#[test]
fn a_prunable_registration_is_not_a_checkout_and_the_merge_lands_by_the_ref() {
    // The `409 target_checked_out` arm must be about a working tree, and a registration git calls **prunable**
    // has none: the directory is gone and `git worktree prune` has not run. Counting it would refuse a merge the
    // operator can perform, with advice ("check out another branch in that worktree") that cannot be followed —
    // and this build leaves such registrations behind routinely. Dropping the
    // `prunable` filter in `checkout_of` turns this test from a `200` into that refusal.
    let fixture = Fixture::new("merge-pruned-registration");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    git(&fixture.repo, &["checkout", "--quiet", "-b", "elsewhere"]);
    let other = fixture.projects.join("gone-checkout");
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
    std::fs::remove_dir_all(&other).expect("the checkout's directory is removed");
    assert!(
        git(&fixture.repo, &["worktree", "list", "--porcelain"]).contains("prunable "),
        "the fixture did not leave a prunable registration"
    );

    let merged = fixture
        .run("main")
        .expect("a prunable registration is not a checkout");

    assert_eq!(merged.merged_commit, tip);
    assert_eq!(fixture.rev("refs/heads/main"), tip);
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

// --- the two strategies ---------------------------------------------------------------------------

#[test]
fn a_merge_makes_one_commit_with_two_parents_the_old_target_first() {
    // The `merge` strategy's whole meaning, and the assertion that separates it from `ff-only`: a branch that
    // *could* be fast-forwarded still gets a commit whose parents are the old target and the branch's tip. Two
    // commits on the branch, so "the tip" is not "one commit after the base", and the old target is asserted
    // as the **first** parent because that is what makes the target's own history first-parent-readable.
    let fixture = Fixture::new("strategy-merge");
    fixture.commit("a.txt", b"a\n", "one file");
    fixture.commit("b.txt", b"b\n", "a second file");
    let tip = fixture.rev(&fixture.branch());
    let before = fixture.rev("refs/heads/main");

    let merged = fixture
        .land_by(Strategy::Merge, None)
        .expect("a merge commit lands");

    assert_eq!(merged.strategy, "merge");
    assert_eq!(merged.target, "main");
    assert_ne!(
        merged.merged_commit, tip,
        "a merge commit is not the branch's tip"
    );
    assert_ne!(merged.merged_commit, before);
    assert_eq!(fixture.rev("refs/heads/main"), merged.merged_commit);
    assert_eq!(fixture.parents("refs/heads/main"), vec![before, tip]);
    // The commit this build wrote is holosmith's, not the machine's: `snapshot::IDENTITY`, carried by `-c` on the
    // command rather than read from a git config (a fresh container or a bare server has none). Dropping the two
    // `-c user.*` pairs makes this assertion name whoever the machine's config does.
    assert_eq!(
        git(
            &fixture.repo,
            &["log", "-1", "--format=%an <%ae>", "refs/heads/main"]
        ),
        "holosmith <holosmith@localhost>"
    );
    // The working tree moved with the ref: the ordinary path, so the operator's checkout holds the work.
    assert_eq!(
        std::fs::read(fixture.repo.join("b.txt")).expect("the file is in the checkout"),
        b"b\n"
    );
    assert_eq!(fixture.status(&fixture.repo), "");
    // Rule 2's last sentence, measured: `<data>/merge` is empty afterwards.
    assert_eq!(
        fixture.temporary(),
        Vec::<String>::new(),
        "a worktree was left behind"
    );
}

#[test]
fn a_squash_makes_one_commit_whose_parent_is_the_old_target_and_whose_tree_is_the_branch_s() {
    // "Squash a three-commit session into one": three commits on the branch, **one** new commit on the target,
    // one parent, and — the assertion `git log --oneline` cannot make — the branch's **tree**. A squash that
    // lost a hunk would still be one commit with one parent.
    let fixture = Fixture::new("strategy-squash");
    fixture.commit("a.txt", b"a\n", "one file");
    fixture.commit("b.txt", b"b\n", "a second file");
    fixture.commit("c.txt", b"c\n", "a third file");
    let tip = fixture.rev(&fixture.branch());
    let before = fixture.rev("refs/heads/main");
    assert_eq!(
        git(
            &fixture.repo,
            &["rev-list", "--count", &format!("{before}..{tip}")]
        ),
        "3"
    );

    let merged = fixture
        .land_by(Strategy::Squash, None)
        .expect("a squashed commit lands");

    assert_eq!(merged.strategy, "squash");
    assert_ne!(merged.merged_commit, tip);
    assert_eq!(fixture.rev("refs/heads/main"), merged.merged_commit);
    assert_eq!(fixture.parents("refs/heads/main"), vec![before.clone()]);
    assert_eq!(
        fixture.tree("refs/heads/main"),
        fixture.tree(&fixture.branch()),
        "the squash's tree is not the branch's tree"
    );
    // One commit, not three: the count of commits above the old target is the strategy's own promise.
    assert_eq!(
        git(
            &fixture.repo,
            &["rev-list", "--count", &format!("{before}..refs/heads/main")]
        ),
        "1"
    );
    // The default message, because git has none for a squash: the sentence the build writes, naming both refs.
    assert_eq!(
        git(
            &fixture.repo,
            &["log", "-1", "--format=%s", "refs/heads/main"]
        ),
        format!("Squash {} into main", fixture.branch())
    );
    // The identity too, on this strategy's own commit: `git commit` is the command that would otherwise take the
    // machine's config, and the same `-c` pair carries it.
    assert_eq!(
        git(
            &fixture.repo,
            &["log", "-1", "--format=%an <%ae>", "refs/heads/main"]
        ),
        "holosmith <holosmith@localhost>"
    );
    assert_eq!(fixture.temporary(), Vec::<String>::new());
}

#[test]
fn the_operator_s_message_is_the_commit_s_and_its_absence_is_the_build_s_one_sentence() {
    // Two rules in one place because they are one decision: a request's `message` is the commit's prose
    // **verbatim**, and no message means the build's own sentence naming both refs. Git's default is not
    // available here and that is a measurement: with no `-m` a detached worktree merging a fully qualified ref
    // writes `Merge branch 'refs/heads/agent/<id>' into HEAD`, which describes this module's scratch directory
    // rather than the operator's branch.
    let fixture = Fixture::new("strategy-message");
    fixture.commit("a.txt", b"a\n", "one file");
    fixture
        .land_by(Strategy::Merge, None)
        .expect("a merge commit lands");
    assert_eq!(
        git(
            &fixture.repo,
            &["log", "-1", "--format=%s", "refs/heads/main"]
        ),
        format!("Merge {} into main", fixture.branch())
    );

    let named = Fixture::new("strategy-message-named");
    named.commit("a.txt", b"a\n", "one file");
    named
        .land_by(Strategy::Merge, Some("Land the session's work"))
        .expect("a merge commit lands");
    assert_eq!(
        git(
            &named.repo,
            &["log", "-1", "--format=%s", "refs/heads/main"]
        ),
        "Land the session's work"
    );

    let squashed = Fixture::new("strategy-message-squash");
    squashed.commit("a.txt", b"a\n", "one file");
    squashed
        .land_by(Strategy::Squash, None)
        .expect("a squashed commit lands");
    assert_eq!(
        git(
            &squashed.repo,
            &["log", "-1", "--format=%s", "refs/heads/main"]
        ),
        format!("Squash {} into main", squashed.branch())
    );

    let one = Fixture::new("strategy-message-squash-named");
    one.commit("a.txt", b"a\n", "one file");
    one.land_by(Strategy::Squash, Some("One commit for the review"))
        .expect("a squashed commit lands");
    assert_eq!(
        git(&one.repo, &["log", "-1", "--format=%s", "refs/heads/main"]),
        "One commit for the review"
    );
}

#[test]
fn a_merge_lands_a_branch_the_target_has_already_moved_past() {
    // The case `merge` exists for and `ff-only` refuses: the target has a commit the branch does not. The real
    // merge reconciles both and writes a commit whose two parents are the moved-on target and the branch — which
    // is why the ancestry test must **not** run for this strategy (`a_target_ahead_of_the_branch_is_not_fast_
    // forward` is the same repository shape, answered by the default strategy with a refusal).
    let fixture = Fixture::new("strategy-target-ahead");
    fixture.commit("a.txt", b"a\n", "the session's change");
    fixture.commit_target("t.txt", b"target\n", "the target moves on");
    let tip = fixture.rev(&fixture.branch());
    let before = fixture.rev("refs/heads/main");
    // The default strategy is the same request and is refused, which is what makes this a comparison.
    assert!(matches!(
        fixture.refused("main"),
        MergeError::NotFastForward { .. }
    ));

    let merged = fixture
        .land_by(Strategy::Merge, None)
        .expect("a moved-on target is what a real merge is for");

    assert_eq!(fixture.parents("refs/heads/main"), vec![before, tip]);
    assert_eq!(fixture.rev("refs/heads/main"), merged.merged_commit);
    // Both halves of the history are in the checkout: the target's file and the branch's.
    assert!(
        fixture.repo.join("a.txt").exists(),
        "the branch's work is missing"
    );
    assert!(
        fixture.repo.join("t.txt").exists(),
        "the target's work is missing"
    );
    assert_eq!(fixture.status(&fixture.repo), "");
}

#[test]
fn a_conflict_is_its_paths_and_nothing_moves() {
    // Rule 3, module level. The two branches touch one path, so the landing cannot reconcile them: the answer is
    // the **paths** (the one refusal with a payload), and "nothing changes" is measured about the project
    // repository — the target ref, the checkout's worktree and its status — because the merge never ran there.
    let fixture = Fixture::new("strategy-conflict");
    fixture.commit("f.txt", b"the branch's line\n", "the session's change");
    fixture.commit_target("f.txt", b"the target's line\n", "the target's change");
    let tip = fixture.rev(&fixture.branch());
    let main = fixture.rev("refs/heads/main");
    let checkout = fixture.status(&fixture.repo);

    for strategy in [Strategy::Merge, Strategy::Squash] {
        match fixture.land_by(strategy, None) {
            Err(MergeError::Conflict { paths }) => assert_eq!(paths, vec!["f.txt"], "{strategy:?}"),
            other => panic!("{strategy:?} on a conflicting branch is not a conflict: {other:?}"),
        }
        assert_eq!(
            fixture.rev("refs/heads/main"),
            main,
            "{strategy:?} moved the target"
        );
        assert_eq!(
            fixture.rev(&fixture.branch()),
            tip,
            "{strategy:?} moved the branch"
        );
        assert_eq!(
            fixture.status(&fixture.repo),
            checkout,
            "{strategy:?} touched the checkout"
        );
        assert_eq!(
            fixture.temporary(),
            Vec::<String>::new(),
            "{strategy:?} left its temporary worktree behind"
        );
    }
    // The conflict *is* a conflict: the same two branches, read by the changes view's own trial, agree.
    assert!(
        git(&fixture.worktree, &["status", "--porcelain"]).is_empty(),
        "the session's worktree was touched"
    );
}

#[test]
fn the_fast_forward_refusal_comes_before_the_checkout_one_and_the_landings_do_not() {
    // Two facts, one repository, and the order is a decision. A branch the target has moved past **and** a target
    // checked out in somebody else's worktree answers `not_fast_forward` by the default strategy — because "check
    // out another branch in that worktree and merge again" would send the operator straight to a second refusal —
    // and `target_checked_out` by `merge`, which asks the checkout question first because asking it after the
    // landing would have built a commit for nothing. the order for its own path is therefore preserved, and
    // the two new strategies' order is deliberate rather than a side effect of one shared function.
    let fixture = Fixture::new("merge-refusal-order");
    fixture.commit("a.txt", b"a\n", "the session's change");
    fixture.commit_target("t.txt", b"target\n", "the target moves on");
    git(&fixture.repo, &["checkout", "--quiet", "-b", "elsewhere"]);
    let other = fixture.projects.join("a-third-checkout");
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

    assert!(
        matches!(fixture.refused("main"), MergeError::NotFastForward { .. }),
        "the default strategy answered about the checkout before asking about the branch"
    );
    match fixture.land_by(Strategy::Merge, None) {
        Err(MergeError::CheckedOutElsewhere { target, path }) => {
            assert_eq!(target, "main");
            assert_eq!(path, other);
        }
        other => panic!("a landing into a checkout it must not touch was not refused: {other:?}"),
    }
    // The refusal did not build anything: no commit, no temporary directory, and the checkout it named is
    // untouched.
    assert_eq!(fixture.temporary(), Vec::<String>::new());
    assert!(
        !other.join("a.txt").exists(),
        "the refusal wrote into the working tree it said it would not touch"
    );
}

#[test]
fn a_squash_keeps_the_branch_a_merge_does_not() {
    // the keep-rule, asked after each strategy — the interaction the handoff's notes named and this story
    // has to decide rather than discover. `Worktree::merged_into` is `git merge-base --is-ancestor agent/<id>
    // <target>`, and a **squash** writes a commit whose only parent is the old target, so the branch's tip is not
    // an ancestor and the branch is *kept* — which is honest (the branch carries the session's own history, and
    // the target does not have it) and which makes rule 5's "the branch is deleted because it is merged" a fact
    //  must decide for this strategy, not a fact it can read off git. A `merge` commit, by contrast, has the
    // branch's tip as a parent, so the same question answers yes and the branch is not kept.
    let merged = Fixture::new("keep-rule-merge");
    merged.commit("a.txt", b"a\n", "the session's change");
    merged
        .land_by(Strategy::Merge, None)
        .expect("a merge commit lands");
    let targets = ["main".to_string(), "master".to_string()];
    match merged
        .worktree()
        .retained(Some(&merged.base), &targets, None)
        .expect("the keep-rule can be asked")
    {
        Retained::Known { branch, .. } => assert_eq!(branch, None),
        other => panic!("a merged branch's keep-rule is not `Known`: {other:?}"),
    }

    let squashed = Fixture::new("keep-rule-squash");
    squashed.commit("a.txt", b"a\n", "the session's change");
    squashed
        .land_by(Strategy::Squash, None)
        .expect("a squashed commit lands");
    match squashed
        .worktree()
        .retained(Some(&squashed.base), &targets, None)
        .expect("the keep-rule can be asked")
    {
        Retained::Known { branch, .. } => assert_eq!(
            branch,
            Some(squashed.branch()),
            "a squashed branch is not an ancestor of the target, so the keep-rule must keep it"
        ),
        other => panic!("a kept branch's keep-rule is not `Known`: {other:?}"),
    }
}

#[test]
fn the_commit_a_merge_landed_is_not_a_branch_to_keep_and_one_that_moved_is() {
    // Rule 5's rule, at the function that answers it (ADR-0065 D6): the commit a request **landed** is
    // not work to keep, whatever the ancestry question says — and the comparison is against the branch's **tip**,
    // so a branch that moved after the merge measured it is the ordinary question's and is kept. Both halves are
    // here because only the pair distinguishes "the request's fact" from "keep everything" and from "delete
    // everything": a `squash` is the strategy the ancestry question answers wrong, and a moved tip is the case a
    // `landed: bool` would get wrong.
    let targets = ["main".to_string(), "master".to_string()];
    let fixture = Fixture::new("keep-rule-landed");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let squashed = fixture
        .land_by(Strategy::Squash, None)
        .expect("a squashed commit lands");
    let tip = fixture.rev(&fixture.branch());
    assert_ne!(
        squashed.merged_commit, tip,
        "the squash wrote the target's commit, not the branch's"
    );
    // The branch is a non-ancestor, so the ordinary question keeps it...
    match fixture
        .worktree()
        .retained(Some(&fixture.base), &targets, None)
        .expect("the keep-rule can be asked")
    {
        Retained::Known { branch, .. } => assert_eq!(branch, Some(fixture.branch())),
        other => panic!("a squashed branch's keep-rule is not `Known`: {other:?}"),
    }
    // ... and the request that landed this exact commit is not what keeps it.
    match fixture
        .worktree()
        .retained(Some(&fixture.base), &targets, Some(&tip))
        .expect("the keep-rule can be asked")
    {
        Retained::Known { branch, .. } => assert_eq!(branch, None),
        other => panic!("a landed branch's keep-rule is not `Known`: {other:?}"),
    }
    // A branch that has moved since is work nothing landed, so it is kept even though a commit of its own was
    // landed once.
    fixture.commit("b.txt", b"b\n", "a commit the merge never saw");
    let moved = fixture.rev(&fixture.branch());
    assert_ne!(moved, tip);
    match fixture
        .worktree()
        .retained(Some(&fixture.base), &targets, Some(&tip))
        .expect("the keep-rule can be asked")
    {
        Retained::Known { branch, .. } => assert_eq!(
            branch,
            Some(fixture.branch()),
            "a branch that moved past the landed commit must be kept"
        ),
        other => panic!("a moved branch's keep-rule is not `Known`: {other:?}"),
    }
}

#[test]
fn a_stale_temporary_worktree_from_a_killed_run_is_removed_and_the_merge_lands() {
    // Rule 2 creates `<data>/merge/<id>`, so a killed merge leaves one there. The story's decision is that such a
    // directory is **removed, never refused**: `409 merge_in_progress` would be an answer about concurrency, and
    // a leftover directory is not a question about concurrency. Git does not know this path (nothing registered
    // it), which is the arm `worktree remove` cannot take and `remove_dir_all` can.
    let fixture = Fixture::new("strategy-stale");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let stale = fixture
        .state
        .config
        .data_root
        .join("merge")
        .join(&fixture.id);
    std::fs::create_dir_all(&stale).expect("the stale directory is creatable");
    std::fs::write(stale.join("leftover.txt"), b"from a killed run")
        .expect("the stale file is writable");
    assert_eq!(fixture.temporary(), vec![fixture.id.clone()]);

    let merged = fixture
        .land_by(Strategy::Merge, None)
        .expect("a stale directory blocks nothing");

    assert_eq!(fixture.rev("refs/heads/main"), merged.merged_commit);
    assert_eq!(fixture.temporary(), Vec::<String>::new());
    assert!(!stale.exists(), "the stale directory survived the merge");

    // The other leftover shape, and the one `remove_dir_all` cannot reach: a **registration** whose directory is
    // gone. Git calls such a registration prunable and `worktree add` refuses a path git holds one for, so
    // `clear`'s unconditional `worktree prune` is what makes this arm work — dropping it turns this half red with
    // git's own "is a missing but already registered worktree".
    let registered = Fixture::new("strategy-registered");
    registered.commit("a.txt", b"a\n", "the session's change");
    let path = registered
        .state
        .config
        .data_root
        .join("merge")
        .join(&registered.id);
    std::fs::create_dir_all(path.parent().expect("the directory has a parent"))
        .expect("the merge directory is creatable");
    git(
        &registered.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            path.to_str().expect("a UTF-8 temp path"),
            "refs/heads/main",
        ],
    );
    std::fs::remove_dir_all(&path).expect("the registered directory is removed");
    // `prunable <reason>`, and not the bare word: a fixture whose own label contained it would make this
    // assertion vacuous, which is how the first version of this test passed over a registration it did not
    // have.
    assert!(
        git(&registered.repo, &["worktree", "list", "--porcelain"]).contains("prunable "),
        "the fixture did not leave a prunable registration"
    );

    let landed = registered
        .land_by(Strategy::Merge, None)
        .expect("a prunable registration is a leftover to clear, not a merge to refuse");

    assert_eq!(registered.rev("refs/heads/main"), landed.merged_commit);
    assert_eq!(registered.temporary(), Vec::<String>::new());
    let listing = git(&registered.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        !listing.contains("prunable "),
        "the prunable registration survived the merge: {listing}"
    );
}

#[test]
fn the_temporary_worktree_is_gone_even_when_the_move_is_refused_after_it_landed() {
    // The acceptance's "removed afterwards even on failure", in its hardest shape: the landing **succeeded** — a
    // commit exists — and the *move* is what fails (`409 target_dirty`, because the branch adds a file the
    // checkout already holds untracked). A build that tore the worktree down after the move would leave
    // `<data>/merge/<id>` behind here, and a build that tore it down in a `?`-short-circuit would too.
    let fixture = Fixture::new("strategy-dirty-after-landing");
    fixture.commit("a.txt", b"the session's version\n", "the session's change");
    let main = fixture.rev("refs/heads/main");
    write(&fixture.repo, "a.txt", b"the operator's work in progress\n");

    match fixture.land_by(Strategy::Merge, None) {
        Err(MergeError::TargetDirty { target, .. }) => assert_eq!(target, "main"),
        other => panic!("a dirty checkout the merge would overwrite is not a refusal: {other:?}"),
    }

    assert_eq!(
        fixture.rev("refs/heads/main"),
        main,
        "the target moved anyway"
    );
    assert_eq!(
        std::fs::read(fixture.repo.join("a.txt")).expect("the operator's file is still there"),
        b"the operator's work in progress\n",
        "the refusal overwrote the operator's file"
    );
    assert_eq!(
        fixture.temporary(),
        Vec::<String>::new(),
        "the temporary worktree outlived a refused move"
    );
}

#[test]
fn no_hook_runs_for_either_strategy() {
    // Rule 2's `-c core.hooksPath=/dev/null`, on the three commands an agent-adjacent repository could otherwise
    // get a program run by: `worktree add` runs `post-checkout`, a `merge` runs `post-merge` and the message
    // hooks, a `commit` runs `pre-commit` and its own pair. Hooks live in the **common** git directory, so a
    // project's hook is reachable from the temporary worktree — which is exactly why the flag is on all three.
    // Dropping it from `landing_prefix` makes both halves of this test red.
    for (strategy, label) in [
        (Strategy::Merge, "hooks-merge"),
        (Strategy::Squash, "hooks-squash"),
    ] {
        let fixture = Fixture::new(label);
        fixture.commit("a.txt", b"a\n", "the session's change");
        let marker = fixture._dir.file(&format!("{label}-hook-ran"));
        for hook in ["post-merge", "post-checkout", "pre-commit", "commit-msg"] {
            let path = fixture.repo.join(".git").join("hooks").join(hook);
            std::fs::create_dir_all(path.parent().expect("the hooks directory has a parent"))
                .expect("the hooks directory is creatable");
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\ntouch '{}-{hook}'\n",
                    marker.to_str().expect("a UTF-8 temp path")
                ),
            )
            .expect("the hook is written");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("the hook is executable");
            }
        }

        fixture
            .land_by(strategy, None)
            .unwrap_or_else(|error| panic!("{strategy:?} did not land: {error:?}"));

        let ran: Vec<String> = std::fs::read_dir(fixture._dir.path())
            .expect("the temp dir is readable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("hook-ran"))
            .collect();
        assert!(
            ran.is_empty(),
            "{strategy:?} ran a project's hook: agent-written code executed on the host: {ran:?}"
        );
    }
}

#[test]
fn a_landing_that_has_nothing_to_do_is_a_no_op_naming_the_target_s_own_commit() {
    // The second-merge shape, one strategy further out: after the first merge the target *is* the branch's
    // history, so a second `merge` is "Already up to date" and a second `squash` stages nothing at all. Both are
    // answered with the target's own commit rather than with a refusal for a request that is already satisfied —
    // and the `squash` half is the one that needs a measurement, because `git commit` **fails** when the index
    // holds no change.
    let fixture = Fixture::new("strategy-noop");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let first = fixture
        .land_by(Strategy::Merge, None)
        .expect("the first merge lands");
    let reflog = git(&fixture.repo, &["reflog", "show", "refs/heads/main"]);

    let second = fixture
        .land_by(Strategy::Merge, None)
        .expect("a second merge is not an error");
    assert_eq!(second.merged_commit, first.merged_commit);
    assert_eq!(
        git(&fixture.repo, &["reflog", "show", "refs/heads/main"]),
        reflog,
        "the no-op wrote the ref"
    );

    let third = fixture
        .land_by(Strategy::Squash, None)
        .expect("a squash with nothing to squash is not an error");
    assert_eq!(third.merged_commit, first.merged_commit);
    assert_eq!(
        git(&fixture.repo, &["reflog", "show", "refs/heads/main"]),
        reflog,
        "the squash no-op wrote the ref"
    );
    assert_eq!(fixture.temporary(), Vec::<String>::new());
}

#[test]
fn two_merges_of_one_project_serialise_and_both_land() {
    // Rule 2's "one merge per project at a time", measured as a client sees it: two sessions of **one**
    // repository merge at the same instant from a barrier, and both land — the second waited for the first, and
    // its own landing was built from the target the first had already produced. Without the lock both read the
    // same `old`, one of them writes the ref first, and the other is refused (`not_fast_forward`) because its
    // landing descends from a commit the target no longer holds. `Merges`' identity is the deterministic half of
    // this (see the unit test); this is the half an operator would notice.
    let fixture = Fixture::new("strategy-concurrent");
    fixture.commit("a.txt", b"a\n", "the first session's change");
    let (other_id, other_path) = fixture.other_session("concurrent");
    write(&other_path, "b.txt", b"b\n");
    git(&other_path, &["add", "b.txt"]);
    git(
        &other_path,
        &["commit", "--quiet", "-m", "the second session's change"],
    );

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let targets = vec!["main".to_string(), "master".to_string()];
    let land = |id: &str| {
        let request = Request {
            target: "main".to_string(),
            strategy: Strategy::Merge,
            message: None,
        };
        let worktree = Worktree::of(&fixture.projects, "holosmith", id);
        let merges = fixture.state.merges.clone();
        let data_root = fixture.state.config.data_root.clone();
        let targets = targets.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            merge(&worktree, &request, &targets, &merges, &data_root)
        })
    };
    let first = land(&fixture.id);
    let second = land(&other_id);
    let first = first.join().expect("the thread finishes");
    let second = second.join().expect("the thread finishes");

    let (first, second) = (
        first.unwrap_or_else(|error| panic!("a concurrent merge was refused: {error:?}")),
        second.unwrap_or_else(|error| panic!("a concurrent merge was refused: {error:?}")),
    );
    // Both landed, in some order: the target and its first parent are exactly the two merge commits the two
    // requests produced. One landing on top of the other is the whole shape of "they serialised" — and the
    // second parent of the target is the second branch's tip, so neither landing swallowed the other's work.
    let head = fixture.rev("refs/heads/main");
    let parents = fixture.parents("refs/heads/main");
    assert_eq!(
        parents.len(),
        2,
        "main is not a two-parent merge commit: {parents:?}"
    );
    let mut produced = vec![first.merged_commit.clone(), second.merged_commit.clone()];
    let mut written = vec![head.clone(), parents[0].clone()];
    produced.sort();
    written.sort();
    assert_eq!(written, produced, "one of the two merges was lost");
    assert_eq!(fixture.temporary(), Vec::<String>::new());
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
async fn every_bad_request_moves_no_ref() {
    // The acceptance's first two bullets and the request's own refusals, at the wire: the codes are the machine's
    // and the refs are the repository's, and neither a refused strategy nor a bad target moves anything.
    // `delete_session: true` used to be one of these (`422 unsupported`) and is rule 5 now, so it left this table
    // for `delete_session_runs_the_delete_flow_and_the_answer_keeps_three_keys`.
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
        (r#"{"target":"main","strategy":"rebase"}"#, "bad_strategy"),
        // A `strategy` that is not a string at all is the *body's* shape rather than a value outside the
        // vocabulary, so it is `bad_body` — the same answer the create gives a numeric `model`. It was
        // `bad_strategy`, where every strategy check was a string comparison; the change is
        // deliberate and ADR-0064 records it.
        (r#"{"target":"main","strategy":3}"#, "bad_body"),
        (r#"{"target":"main","message":""}"#, "bad_body"),
        (r#"{"target":"main","force":true}"#, "unknown_field"),
        (r#"{"strategy":"ff-only"}"#, "bad_body"),
    ] {
        let response = post_merge(served.addr, &fixture.id, body).await;
        assert_eq!(response.status, 422, "{body}: {}", response.body);
        assert_eq!(envelope(&response)["code"], code, "{body}");
        assert_eq!(response.header("content-type"), Some("application/json"));
        // The envelope is `{code, message}` for every error that is not a conflict: `conflicts` is skipped when
        // it is absent, so the shape a client already reads did not change.
        assert!(
            envelope(&response).get("conflicts").is_none(),
            "a non-conflict carried a conflicts key: {body}"
        );
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
async fn a_squash_at_the_wire_answers_the_strategy_that_ran_and_moves_the_target() {
    // The story's demo as an assertion: `{target:"main",strategy:"squash"}` over three commits is a `200` whose
    // `strategy` echoes the request (not the default), whose `merged_commit` is the new commit, and whose
    // repository state is one commit with the branch's tree — read by the test from the repository.
    let fixture = Fixture::new("merge-wire-squash");
    fixture.commit("a.txt", b"a\n", "one file");
    fixture.commit("b.txt", b"b\n", "a second file");
    fixture.commit("c.txt", b"c\n", "a third file");
    let before = fixture.rev("refs/heads/main");
    let branch_tree = fixture.tree(&fixture.branch());
    let served = fixture.served().await;

    let response = post_merge(
        served.addr,
        &fixture.id,
        r#"{"target":"main","strategy":"squash"}"#,
    )
    .await;

    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body = envelope(&response);
    assert_eq!(body["strategy"], "squash");
    assert_eq!(body["target"], "main");
    assert_eq!(body.as_object().expect("an object").len(), 3);
    assert_eq!(fixture.rev("refs/heads/main"), body["merged_commit"]);
    assert_eq!(fixture.parents("refs/heads/main"), vec![before]);
    assert_eq!(fixture.tree("refs/heads/main"), branch_tree);
    assert_eq!(
        std::fs::read(fixture.repo.join("c.txt")).expect("the file is in the checkout"),
        b"c\n"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_merge_at_the_wire_answers_two_parents_and_the_operator_s_message() {
    let fixture = Fixture::new("merge-wire-merge");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    let before = fixture.rev("refs/heads/main");
    let served = fixture.served().await;

    let response = post_merge(
        served.addr,
        &fixture.id,
        r#"{"target":"main","strategy":"merge","message":"Land the session"}"#,
    )
    .await;

    assert_eq!(response.status, 200, "{}", response.body);
    let body = envelope(&response);
    assert_eq!(body["strategy"], "merge");
    assert_eq!(fixture.rev("refs/heads/main"), body["merged_commit"]);
    assert_eq!(fixture.parents("refs/heads/main"), vec![before, tip]);
    assert_eq!(
        git(
            &fixture.repo,
            &["log", "-1", "--format=%s", "refs/heads/main"]
        ),
        "Land the session"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_conflict_at_the_wire_is_a_409_whose_body_names_the_paths() {
    // Rule 3 as a client reads it: a `409`, the code `conflict`, the **paths** in an additive key, and a sentence
    // that says nothing changed and names the next step. "Nothing changes" is measured on the repository, and the
    // temporary directory is measured empty — the refusal is the *abort*, and nothing of the attempt survives.
    let fixture = Fixture::new("merge-wire-conflict");
    fixture.commit("f.txt", b"the branch's line\n", "the session's change");
    fixture.commit_target("f.txt", b"the target's line\n", "the target's change");
    let main = fixture.rev("refs/heads/main");
    let served = fixture.served().await;

    let response = post_merge(
        served.addr,
        &fixture.id,
        r#"{"target":"main","strategy":"merge"}"#,
    )
    .await;

    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body = envelope(&response);
    assert_eq!(body["code"], "conflict");
    assert_eq!(body["conflicts"], serde_json::json!(["f.txt"]));
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("nothing was merged")),
        "the sentence does not say nothing happened: {body}"
    );
    assert_eq!(fixture.rev("refs/heads/main"), main, "the target moved");
    assert_eq!(
        fixture.status(&fixture.repo),
        "",
        "the checkout was touched"
    );
    assert_eq!(fixture.temporary(), Vec::<String>::new());
    served.finish().await.expect("the server stops");
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
async fn a_merge_leaves_the_changes_view_unchanged() {
    // ADR-0063's D7, and the JSON half is deliberately **not** the proof: `ahead`, `behind` and `mergeable` are
    // the same after a fast-forward as before it, and they have to be — the merge moves the *target*, `ahead` is
    // `rev-list --count <base>..agent/<id>`, and `behind` was already `0` because a fast-forward requires the
    // target to be an ancestor of the branch. A build that wrote nothing would pass a JSON-only assertion, so the
    // contrast comes from the repository: the summary is unchanged **and** the target is now the branch's tip,
    // which is the fact a merge that did nothing cannot produce.
    let fixture = Fixture::new("merge-wire-changes");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    let served = fixture.served().await;
    let before: Value = serde_json::from_str(
        &fetch(
            served.addr,
            &format!("/api/sessions/{}/changes", fixture.id),
        )
        .await
        .body,
    )
    .expect("the summary is JSON");
    assert_eq!(before["ahead"], 1);
    assert_eq!(before["behind"], 0);
    assert_eq!(before["mergeable"], true);

    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 200, "{}", response.body);

    let after: Value = serde_json::from_str(
        &fetch(
            served.addr,
            &format!("/api/sessions/{}/changes", fixture.id),
        )
        .await
        .body,
    )
    .expect("the summary is JSON");
    // The merge happened ...
    assert_eq!(
        fixture.rev("refs/heads/main"),
        tip,
        "the target did not move"
    );
    assert_eq!(fixture.rev(&fixture.branch()), tip);
    // ... and the branch's summary is unchanged field for field, which is why nothing about the row had to be
    // written.
    for field in [
        "branch",
        "base_commit",
        "head",
        "target",
        "ahead",
        "behind",
        "mergeable",
        "files",
    ] {
        assert_eq!(
            after[field], before[field],
            "{field} changed because of a merge"
        );
    }
    assert_eq!(after["dirty"], false);
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_target_checked_out_in_another_worktree_is_a_409_at_the_wire() {
    // The third answer to rule 4's two-arm question, at the status-and-code level: the project's own checkout
    // leaves the target and a second worktree takes it, which is what a session's worktree holding `main` looks
    // like. The module's own test asserts the variant; this one asserts what a client is told, and that the
    // working tree the refusal said it would not touch really is untouched.
    let fixture = Fixture::new("merge-wire-elsewhere");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let main = fixture.rev("refs/heads/main");
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
    let served = fixture.served().await;
    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 409, "{}", response.body);
    let body = envelope(&response);
    assert_eq!(body["code"], "target_checked_out");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("nothing was merged")),
        "the sentence does not say nothing happened: {body}"
    );
    assert_eq!(
        fixture.rev("refs/heads/main"),
        main,
        "the refusal moved the target"
    );
    assert!(
        !other.join("a.txt").exists(),
        "the refusal wrote into the working tree it said it would not touch"
    );
    served.finish().await.expect("the server stops");
}

// --- rule 1's two guards, and rule 5's `delete_session` -------------------------------------------

/// One state change through the table's own function, the way `tests/delete.rs` drives a row it must hold in a
/// state no request of its own would produce: a running turn and a `deleting` row are both things the API
/// cannot be *asked* for without a container or a task.
async fn drive(state: &AppState, id: &str, trigger: Trigger, reason: Option<&str>) {
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(id, trigger, reason)
        .await
        .expect("the move is in the table");
}

/// The row's status, read under the one lock the repository is read with.
fn status_of(state: &AppState, id: &str) -> Status {
    state
        .database
        .lock()
        .expect("the lock is not poisoned")
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the row is there")
        .status
}

/// Wait (bounded) until the row is gone, which is what a delete's removal off the request's task reaches.
async fn wait_gone(served: &Served, id: &str) -> bool {
    for _ in 0..100 {
        let response = request(
            served.addr,
            &format!(
                "GET /api/sessions/{id} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        if response.status == 404 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Whether the merge's temporary directory exists at all — the measure of "a refused merge costs no directory",
/// which is stronger than "the directory is empty": `land` is what makes it, and a refusal must not reach it.
fn temporary_exists(fixture: &Fixture) -> bool {
    fixture.state.config.data_root.join("merge").exists()
}

#[tokio::test]
async fn a_running_turn_refuses_the_merge_and_moves_nothing() {
    // Rule 1's first bullet. The row is `running` through the table's own function rather than by a raw write,
    // and what the refusal is measured by is the repository: the target and the branch where they were, the
    // agent's file untouched, and no temporary directory at all.
    let fixture = Fixture::new("merge-guard-running");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    let before = fixture.rev("refs/heads/main");
    drive(&fixture.state, &fixture.id, Trigger::PiReady, None).await;
    drive(&fixture.state, &fixture.id, Trigger::Prompt, None).await;
    assert_eq!(status_of(&fixture.state, &fixture.id), Status::Running);

    let served = fixture.served().await;
    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body = envelope(&response);
    assert_eq!(body["code"], "session_running");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("nothing was merged")),
        "the sentence does not say nothing happened: {body}"
    );
    assert_eq!(fixture.rev("refs/heads/main"), before, "the target moved");
    assert_eq!(fixture.rev(&fixture.branch()), tip, "the branch moved");
    assert_eq!(
        std::fs::read(fixture.worktree.join("a.txt")).expect("the agent's file"),
        b"a\n"
    );
    assert!(
        !temporary_exists(&fixture),
        "a refused merge made <data>/merge"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn every_status_but_running_still_merges() {
    // Rule 1's **negative half**, and the reason it exists: `while a turn runs` is `== Running`, and a guard
    // written `!= Idle` would refuse four of these five. One of them is not hypothetical — a provision that
    // fails leaves the row `error`, which is exactly the session whose branch an operator wants to merge — and
    // `deleting` is the operator's own request rather than a turn.
    let cases: [(&str, &[Trigger]); 5] = [
        ("starting", &[]),
        ("idle", &[Trigger::PiReady]),
        ("stopped", &[Trigger::PiReady, Trigger::IdleStop]),
        ("error", &[Trigger::ProvisioningFailed]),
        ("deleting", &[Trigger::Delete]),
    ];
    for (label, moves) in cases {
        let fixture = Fixture::new(&format!("merge-guard-{label}"));
        fixture.commit("a.txt", b"a\n", "the session's change");
        for trigger in moves {
            drive(&fixture.state, &fixture.id, *trigger, None).await;
        }
        assert_ne!(
            status_of(&fixture.state, &fixture.id),
            Status::Running,
            "{label}"
        );
        let tip = fixture.rev(&fixture.branch());
        let served = fixture.served().await;
        let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
        assert_eq!(response.status, 200, "{label}: {}", response.body);
        assert_eq!(
            fixture.rev("refs/heads/main"),
            tip,
            "{label}: the merge did not move the target"
        );
        served.finish().await.expect("the server stops");
    }
}

#[tokio::test]
async fn an_uncommitted_file_refuses_the_merge_and_the_offer_is_a_commit() {
    // Rule 1's second bullet. The file is *untracked* and in the session's own worktree — the definition of
    // work `changes::dirty` uses, and the subject rule 1 names — so the refusal is a pre-check on this request's
    // own facts rather than the `target_dirty` shape (ADR-0065 D3). The commit halves are what the Changes
    // sheet's offer will send; the sentence has to name one, or the UI would be inventing it.
    let fixture = Fixture::new("merge-guard-dirty");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let before = fixture.rev("refs/heads/main");
    write(&fixture.worktree, "wip.txt", b"still being written\n");

    let served = fixture.served().await;
    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body = envelope(&response);
    assert_eq!(body["code"], "dirty_worktree");
    let message = body["message"].as_str().expect("a sentence");
    assert!(message.contains("nothing was merged"), "{message}");
    // The needle is the offer, not the word "commit": "uncommitted" contains it, so a guard on the bare word
    // stays green when the whole next-step half is deleted — measured, and this assertion's own reason for
    // existing (review S1).
    assert!(message.contains("ask the agent to commit"), "{message}");
    assert_eq!(fixture.rev("refs/heads/main"), before, "the target moved");
    assert_eq!(
        std::fs::read(fixture.worktree.join("wip.txt")).expect("the agent's file"),
        b"still being written\n",
        "the refusal touched the work it refused to merge"
    );
    assert!(
        !temporary_exists(&fixture),
        "a refused merge made <data>/merge"
    );
    served.finish().await.expect("the server stops");

    // ... and the same request lands once the work is a commit, which is what makes the refusal the file's.
    git(&fixture.worktree, &["add", "wip.txt"]);
    git(&fixture.worktree, &["commit", "--quiet", "-m", "the work"]);
    let served = fixture.served().await;
    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 200, "{}", response.body);
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn an_ignored_file_does_not_refuse_the_merge() {
    // `changes::dirty` is the one definition of work (`--untracked-files=normal`), so an ignored file is not
    // work. A guard that asked `git status --short --ignored` or `ls-files --others` would refuse here, which is
    // why the case exists: every build writes ignored files.
    let fixture = Fixture::new("merge-guard-ignored");
    fixture.commit(".gitignore", b"*.log\n", "ignore the logs");
    write(&fixture.worktree, "build.log", b"noise\n");
    let tip = fixture.rev(&fixture.branch());

    let served = fixture.served().await;
    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(fixture.rev("refs/heads/main"), tip);
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_session_with_no_worktree_is_refused_before_the_dirty_check() {
    // The guard order, measured: `changes::basis` is asked before `changes::dirty`, so a worktree that is gone
    // answers this route's `409 no_worktree` rather than a `500` from `git status` in a directory that is not
    // there. The row keeps its `base_commit` and loses only the directory, which is a `rm -rf` or a teardown's
    // leftovers.
    let fixture = Fixture::new("merge-guard-gone");
    fixture.commit("a.txt", b"a\n", "the session's change");
    std::fs::remove_dir_all(&fixture.worktree).expect("the worktree is removable");

    let served = fixture.served().await;
    let response = post_merge(served.addr, &fixture.id, r#"{"target":"main"}"#).await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "no_worktree");
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn delete_session_runs_the_delete_flow_and_the_answer_keeps_three_keys() {
    // Rule 5's field, at the wire: the row is gone afterwards, and the answer is still exactly rule 6's three
    // keys — a merge that also deleted the session says nothing additive (ADR-0065 D7). This fixture's backend
    // keeps no repository, so what the *branch* becomes after a landing is `case_merge_guards`' to measure; what
    // this asserts is that the field reaches the delete flow and that the delete flow runs.
    let fixture = Fixture::new("merge-wire-delete");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());

    let served = fixture.served().await;
    let response = post_merge(
        served.addr,
        &fixture.id,
        r#"{"target":"main","strategy":"merge","delete_session":true}"#,
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    // A `merge` writes a commit, so what the answer names is the target's *new* commit — the same commit the
    // target points at, read by the test — and the branch's tip is one of its parents.
    assert_eq!(body["merged_commit"], fixture.rev("refs/heads/main"));
    assert!(fixture.parents("refs/heads/main").contains(&tip));
    assert_eq!(body["target"], "main");
    assert_eq!(body["strategy"], "merge");
    assert_eq!(
        body.as_object().expect("an object").len(),
        3,
        "the answer gained a key: {body}"
    );
    assert!(
        wait_gone(&served, &fixture.id).await,
        "the session was not deleted"
    );
    served.finish().await.expect("the server stops");
}
