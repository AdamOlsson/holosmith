//! The changes view's summary over a real repository (spec item 12).
//!
//! Every acceptance bullet of the story is a *git* fact, so these tests own a real repository in a
//! `TempDir` — a first commit on `main`, one worktree, one branch `agent/<id>` — and move refs between two
//! assertions. No Docker, no daemon, no backend and (except for the last group) no request: the module is a
//! reader, and item 12's sentence is that all of its work runs on the host in the project repository.
//!
//! The one thing a `TempDir` cannot prove is the *wire* — that the endpoint answers the eleven keys, the
//! envelope's `404` and the story's `409` — so the last group boots the real router over the same repository.
//! What neither half measures is a container, and that is the point: the changes view asks none.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::Method;
use holosmith::changes::{Changes, ChangesError, DIFF_CAP, DiffError, touches_ci};
use holosmith::clock;
use holosmith::config::Config;
use holosmith::http::{self, AppState};
use holosmith::sessions::NewSession;
use holosmith::worktree::Worktree;
use serde_json::Value;

mod support;
use support::{Served, TempDir, fetch, git, head, repository_on, request, state_in};

/// The drain deadline each served test gives `serve`.
const GRACE: Duration = Duration::from_millis(100);

/// A binary file: git calls a file binary when a NUL appears in the first 8000 bytes, so one NUL is the whole
/// fixture.
const BINARY: &[u8] = b"\x00\x01\x02binary\n";

/// One session's repository: the data root, the projects root, the repository, the worktree and the row.
///
/// Built the way the provisioner builds it — `git worktree add -b agent/<id> <path> <base>` and one
/// `set_worktree` — and then driven directly, which is what lets a test assert a count against `git rev-list`
/// it runs itself rather than against the number the code just printed.
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
        Self::with_base(label, &[])
    }

    /// The same, with `files` committed on `main` **before** the branch is made — so they are part of the base
    /// and the branch's changes to them are modifications rather than additions.
    fn with_base(label: &str, files: &[(&str, &[u8])]) -> Self {
        let dir = TempDir::new(label);
        let projects = dir.file("projects");
        let repo = projects.join("holosmith");
        repository_on(&repo, "main");
        for (path, contents) in files {
            let full = repo.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("the directory is creatable");
            }
            std::fs::write(&full, contents).expect("the file is written");
            git(&repo, &["add", path]);
        }
        if !files.is_empty() {
            git(
                &repo,
                &["commit", "--quiet", "-m", "the files the base holds"],
            );
        }
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
                        client_key: Some(format!("changes-{label}")),
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

    /// The answer `GET /api/sessions/:id/changes` would give, with the machine's default targets.
    fn read(&self) -> Changes {
        self.read_with(&["main".to_string(), "master".to_string()])
    }

    fn read_with(&self, targets: &[String]) -> Changes {
        Changes::read(&self.worktree(), &self.base, targets, &[]).expect("the changes view answers")
    }

    /// The same answer with the machine's publish remotes, for the tests of the sheet's second picker.
    fn read_with_remotes(&self, targets: &[String], remotes: &[String]) -> Changes {
        Changes::read(&self.worktree(), &self.base, targets, remotes)
            .expect("the changes view answers")
    }

    fn read_err(&self) -> ChangesError {
        Changes::read(
            &self.worktree(),
            &self.base,
            &["main".to_string(), "master".to_string()],
            &[],
        )
        .expect_err("the changes view refuses")
    }

    /// Write `path` in the worktree and commit it on the session branch.
    fn commit(&self, path: &str, contents: &[u8], message: &str) {
        let full = self.worktree.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("the directory is creatable");
        }
        std::fs::write(&full, contents).expect("the file is written");
        git(&self.worktree, &["add", path]);
        git(&self.worktree, &["commit", "--quiet", "-m", message]);
    }

    /// Write `path` in the main checkout and commit it there — the target moving on.
    fn commit_target(&self, path: &str, contents: &[u8], message: &str) {
        let full = self.repo.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("the directory is creatable");
        }
        std::fs::write(&full, contents).expect("the file is written");
        git(&self.repo, &["add", path]);
        git(&self.repo, &["commit", "--quiet", "-m", message]);
    }

    /// `git status --porcelain` in a directory, as text — what "the working tree did not move" means.
    fn status(&self, dir: &Path) -> String {
        git(dir, &["status", "--porcelain"])
    }

    async fn served(&self) -> Served {
        Served::start(http::router(self.state.clone()), GRACE).await
    }
}

/// One file in the answer, for the tests that assert on a path.
fn file<'a>(changes: &'a Changes, path: &str) -> &'a holosmith::changes::FileChange {
    changes
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} is not in {:?}", changes.files))
}

#[test]
fn the_summary_names_every_field_and_counts_both_sides() {
    // The story's demo, as an assertion: two commits on the branch and one on the target. `ahead` and
    // `behind` are compared with `git rev-list --count` run here, not with the numbers the code printed.
    //
    // The counts are deliberately **unequal** (2 and 1): with one commit each way a test cannot tell a
    // swapped range from the right one, so `behind` would pass against `rev-list <target>..<branch>` — the
    // measurement this bullet exists to make. Verified by mutating the range and watching this go red.
    let fixture = Fixture::new("changes-summary");
    fixture.commit("a.txt", b"a\n", "one file");
    fixture.commit("c.txt", b"c\n", "a second file");
    fixture.commit_target("b.txt", b"b\n", "the target moves on");
    let changes = fixture.read();

    assert_eq!(changes.branch, fixture.branch());
    assert_eq!(changes.base_commit, fixture.base);
    assert_eq!(
        changes.head,
        git(&fixture.repo, &["rev-parse", &fixture.branch()]),
        "head is not the branch's tip"
    );
    assert_eq!(changes.target.as_deref(), Some("main"));
    assert_eq!(
        changes.ahead,
        git(
            &fixture.repo,
            &[
                "rev-list",
                "--count",
                &format!("{}..{}", fixture.base, fixture.branch())
            ]
        )
        .parse::<u64>()
        .expect("a count")
    );
    assert_eq!(changes.ahead, 2, "two commits on the branch");
    // One commit on the target that the branch lacks — and the numbers differ, so `behind` cannot be the
    // same question as `ahead` and pass.
    assert_eq!(changes.behind, Some(1));
    assert_ne!(changes.behind, Some(changes.ahead));
    assert!(!changes.dirty);
    assert_eq!(changes.mergeable, Some(true));
    assert_eq!(changes.mergeable_reason, None);
    assert_eq!(changes.files.len(), 2, "{:?}", changes.files);
    let a = file(&changes, "a.txt");
    assert_eq!(a.status, "A");
    assert_eq!(a.additions, Some(1));
    assert_eq!(a.deletions, Some(0));
    assert!(!changes.touches_ci);
}

#[test]
fn a_modified_file_counts_the_lines_git_counts() {
    // The file is in the *base*, so the branch's rewrite is a modification: `one` unchanged, `two` replaced,
    // `three` added — two lines in, one line out, which is what git counts.
    let fixture = Fixture::with_base("changes-modified", &[("a.txt", b"one\ntwo\n")]);
    std::fs::write(fixture.worktree.join("a.txt"), b"one\nchanged\nthree\n")
        .expect("the file is written");
    git(&fixture.worktree, &["add", "a.txt"]);
    git(&fixture.worktree, &["commit", "--quiet", "-m", "rewrite"]);
    let changes = fixture.read();
    let a = file(&changes, "a.txt");
    assert_eq!(a.status, "M");
    // Two dots: the base commit's tree against the head's, which is the whole story of the file.
    assert_eq!((a.additions, a.deletions), (Some(2), Some(1)));
}

#[test]
fn an_untracked_file_is_work_and_an_ignored_one_is_not() {
    // The story's fourth acceptance bullet, and `dirty`'s definition: the WIP snapshot's `add -A` would
    // store an untracked file and would not store an ignored one, so the changes view must agree with it.
    let fixture = Fixture::new("changes-dirty");
    fixture.commit(".gitignore", b"ignored.txt\n", "ignore a build artefact");
    assert!(!fixture.read().dirty, "a committed .gitignore is not work");

    std::fs::write(fixture.worktree.join("ignored.txt"), b"x\n").expect("the file is written");
    let ignored = fixture.read();
    assert!(!ignored.dirty, "an ignored file was counted as work");
    assert_eq!(ignored.files.len(), 1, "an ignored file is not in the diff");

    std::fs::write(fixture.worktree.join("untracked.txt"), b"y\n").expect("the file is written");
    let untracked = fixture.read();
    assert!(untracked.dirty, "an untracked file is not work?");
    // ... and it is not in `files[]`, because the diff is between two commits.
    assert_eq!(
        untracked.files.len(),
        1,
        "an untracked file is in the commit diff: {:?}",
        untracked.files
    );
}

#[test]
fn a_repository_config_cannot_hide_an_untracked_file() {
    // `status.showUntrackedFiles = no` is a repository's own opinion about what "work" is, and the explicit
    // `--untracked-files=normal` is what makes the answer the command's rather than the config's. Without the
    // flag this test reads `dirty: false` about a file that is plainly there — which is why the flag is a
    // decision (ADR-0061) and not an inherited default. Mutating `dirty()` to drop it makes this go red.
    let fixture = Fixture::new("changes-config-untracked");
    git(
        &fixture.worktree,
        &["config", "status.showUntrackedFiles", "no"],
    );
    std::fs::write(fixture.worktree.join("invisible.txt"), b"x\n").expect("the file is written");
    assert!(
        fixture.read().dirty,
        "a repository config hid the worktree's untracked work"
    );
}

#[test]
fn a_binary_file_has_no_line_counts_rather_than_zero() {
    // `--numstat` prints `-`/`-` for a file it will not count. `0` there would say "no lines changed" about a
    // file that changed entirely, so the field is a null and a client draws a dash.
    let fixture = Fixture::new("changes-binary");
    let full = fixture.worktree.join("logo.bin");
    std::fs::write(&full, BINARY).expect("the file is written");
    git(&fixture.worktree, &["add", "logo.bin"]);
    git(
        &fixture.worktree,
        &["commit", "--quiet", "-m", "a binary file"],
    );
    let changes = fixture.read();
    let logo = file(&changes, "logo.bin");
    assert_eq!(logo.status, "A");
    assert_eq!(logo.additions, None);
    assert_eq!(logo.deletions, None);
}

#[test]
fn a_rename_is_a_delete_and_an_add() {
    // `--no-renames` is the decision: a rename's `{old => new}` spelling is one path cell holding two paths,
    // and a delete plus an add is a truthful account of the lines. This is the test that would fail if the
    // flag were dropped, because git would then print `R100` and a braced path.
    let fixture = Fixture::with_base("changes-rename", &[("old.txt", b"one\ntwo\n")]);
    git(&fixture.worktree, &["mv", "old.txt", "new.txt"]);
    git(&fixture.worktree, &["commit", "--quiet", "-m", "rename"]);
    let changes = fixture.read();
    let statuses: Vec<(&str, &str)> = changes
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status.as_str()))
        .collect();
    assert_eq!(statuses, vec![("new.txt", "A"), ("old.txt", "D")]);
}

#[test]
fn an_empty_branch_has_no_files_and_no_ahead() {
    let fixture = Fixture::new("changes-empty");
    let changes = fixture.read();
    assert_eq!(changes.ahead, 0);
    assert_eq!(changes.head, fixture.base, "the branch is at its base");
    assert!(changes.files.is_empty(), "{:?}", changes.files);
    assert!(!changes.dirty);
    assert_eq!(changes.mergeable, Some(true), "a no-op merge is clean");
    // The target is the base, so there is nothing the branch lacks either.
    assert_eq!(changes.behind, Some(0));
}

#[test]
fn a_ci_file_in_the_diff_is_touches_ci() {
    // Item 12's publish rule, read where it first exists: an **added** workflow counts, because a new
    // workflow is exactly the thing the warning is for.
    let fixture = Fixture::new("changes-ci");
    fixture.commit(
        ".github/workflows/ci.yml",
        b"on: push\njobs: {}\n",
        "a workflow",
    );
    let changes = fixture.read();
    assert!(changes.touches_ci);
    assert!(touches_ci(&changes.files));
    // ... and a file that merely lives in a similar directory is not CI.
    let other = Fixture::new("changes-not-ci");
    other.commit("docs/github.md", b"notes\n", "notes about github");
    assert!(!other.read().touches_ci);
}

#[test]
fn a_conflict_makes_it_unmergeable_and_moves_no_working_tree() {
    // The third acceptance bullet, with the measurement it asks for: `merge-tree --write-tree` answers, and
    // nothing on disk moves — HEAD, the repository's status and the worktree's status are all unchanged.
    let fixture = Fixture::new("changes-conflict");
    fixture.commit("f.txt", b"branch\n", "the branch's version");
    fixture.commit_target("f.txt", b"target\n", "the target's version");
    // The uncommitted file is planted *before* the measurement, so "nothing moved" is about the read.
    std::fs::write(fixture.worktree.join("uncommitted.txt"), b"wip\n")
        .expect("the file is written");
    let before = (
        head(&fixture.repo),
        fixture.status(&fixture.repo),
        fixture.status(&fixture.worktree),
    );

    let changes = fixture.read();
    assert_eq!(changes.mergeable, Some(false), "{changes:?}");
    let reason = changes
        .mergeable_reason
        .as_deref()
        .expect("a conflict names its paths");
    assert!(
        reason.contains("f.txt"),
        "the reason names no path: {reason}"
    );

    assert_eq!(
        (
            head(&fixture.repo),
            fixture.status(&fixture.repo),
            fixture.status(&fixture.worktree)
        ),
        before,
        "the merge trial moved a working tree"
    );
    // The uncommitted file is still untracked, and the branch's tip is still where it was.
    assert_eq!(
        changes.head,
        git(&fixture.repo, &["rev-parse", &fixture.branch()])
    );
}

#[test]
fn a_repository_without_a_target_answers_unknown_rather_than_false() {
    // The P1 lesson of the review, as a test: no target is "I could not ask", and folding it into
    // `false` would say "this branch cannot merge" about a question nobody asked. `behind` is null for the
    // same reason — there is no branch to be behind.
    let fixture = Fixture::new("changes-no-target");
    let changes = fixture.read_with(&["trunk".to_string()]);
    assert_eq!(changes.target, None);
    assert_eq!(changes.behind, None);
    assert_eq!(changes.mergeable, None);
    let reason = changes
        .mergeable_reason
        .as_deref()
        .expect("an unasked question says why");
    assert!(
        reason.contains("trunk"),
        "the reason names no target: {reason}"
    );

    let none = fixture.read_with(&[]);
    assert_eq!(none.target, None);
    assert_eq!(none.mergeable, None);
    assert!(
        none.mergeable_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no merge targets")),
        "{:?}",
        none.mergeable_reason
    );
}

#[test]
fn the_first_existing_target_wins() {
    // `[merge] targets` names both because a machine has one of them; the answer is the first the repository
    // *has*, which is the order the UI preselects.
    let fixture = Fixture::new("changes-first-target");
    let changes = fixture.read_with(&["master".to_string(), "main".to_string()]);
    assert_eq!(changes.target.as_deref(), Some("main"));
    assert_eq!(changes.behind, Some(0));
    // The picker's list is the same walk: `master` is configured and not there, so it is not an option, and the
    // order is the config's — an option the merge would refuse with `422 bad_target` is a button that cannot work.
    assert_eq!(changes.choices.targets, ["main"]);
}

#[test]
fn the_choices_are_the_targets_that_exist_and_the_remotes_as_configured() {
    // The pickers' one key (ADR-0067). Two arms, and each is measurably different from the other:
    //
    // - `targets` is filtered by what the repository has, in the **config's** order, so a second existing branch
    //   appears in the position the config put it in rather than in git's;
    // - `remotes` is the config's list verbatim, because whether the repository has each remote is the publish
    //   route's question and its own sentence.
    let fixture = Fixture::new("changes-choices");
    // A second branch the config also names — `main` is created by the fixture.
    git(&fixture.repo, &["branch", "develop"]);
    let targets = [
        "develop".to_string(),
        "main".to_string(),
        "trunk".to_string(),
    ];
    let remotes = ["origin".to_string(), "backup".to_string()];
    let changes = fixture.read_with_remotes(&targets, &remotes);
    assert_eq!(changes.choices.targets, ["develop", "main"]);
    assert_eq!(changes.choices.remotes, ["origin", "backup"]);
    // `target` is the first of them, which is what a picker preselects: one walk, two answers.
    assert_eq!(changes.target.as_deref(), Some("develop"));
    // The remote list is not filtered, and the empty list is the sheet's "not configured" (it hides Publish).
    assert!(
        fixture
            .read_with_remotes(&targets, &[])
            .choices
            .remotes
            .is_empty()
    );
}

#[test]
fn a_missing_worktree_directory_is_a_refusal() {
    let fixture = Fixture::new("changes-missing");
    std::fs::remove_dir_all(&fixture.worktree).expect("the worktree is removed");
    assert_eq!(
        fixture.read_err(),
        ChangesError::Missing {
            path: fixture.worktree.clone()
        }
    );
}

#[test]
fn a_pruned_worktree_is_a_refusal_even_when_the_directory_is_back() {
    // The story's word is "pruned": git forgets the registration, and a directory that is there anyway is not
    // a worktree — every read inside it would fail with "not a git repository". The refusal names that state
    // rather than letting git's prose leak into a 500.
    let fixture = Fixture::new("changes-pruned");
    git(
        &fixture.repo,
        &[
            "worktree",
            "remove",
            "--force",
            fixture.worktree.to_str().expect("a UTF-8 temp path"),
        ],
    );
    std::fs::create_dir_all(&fixture.worktree).expect("the directory is made again");
    assert_eq!(
        fixture.read_err(),
        ChangesError::Unregistered {
            path: fixture.worktree.clone()
        }
    );
}

#[test]
fn a_missing_project_repository_is_a_refusal() {
    let fixture = Fixture::new("changes-no-repository");
    std::fs::remove_dir_all(&fixture.repo).expect("the repository is removed");
    assert_eq!(
        fixture.read_err(),
        ChangesError::NoRepository {
            repository: fixture.repo.clone()
        }
    );
}

#[test]
fn a_missing_branch_is_a_refusal() {
    // A branch that is not there is not a diff: `head` would have no answer, and every other field is
    // measured from it.
    let fixture = Fixture::new("changes-no-branch");
    git(
        &fixture.repo,
        &[
            "worktree",
            "remove",
            "--force",
            fixture.worktree.to_str().expect("a UTF-8 temp path"),
        ],
    );
    git(&fixture.repo, &["branch", "-D", &fixture.branch()]);
    std::fs::create_dir_all(&fixture.worktree).expect("the directory is made again");
    // Re-registered so the *branch* is the fact under test, not the registration.
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            fixture.worktree.to_str().expect("a UTF-8 temp path"),
        ],
    );
    assert_eq!(
        fixture.read_err(),
        ChangesError::NoBranch {
            branch: fixture.branch()
        }
    );
}

#[test]
fn a_repository_git_cannot_read_is_a_failure_not_a_refusal() {
    // A `.git` that is not a repository is a machine problem, which is the `Git` arm — the one the handler
    // answers as a 500 with git's words in the log, the split `refusal_error` makes for a create.
    let fixture = Fixture::new("changes-unreadable");
    std::fs::remove_dir_all(fixture.repo.join(".git"))
        .expect("the repository's directory is removed");
    std::fs::write(fixture.repo.join(".git"), b"gitdir: /nonexistent\n")
        .expect("the file is written");
    assert!(
        matches!(fixture.read_err(), ChangesError::Git(_)),
        "{:?}",
        fixture.read_err()
    );
}

// --- the wire ---------------------------------------------------------------------------------------------

/// A GET the way the app's own client sends it, with the JSON content type the guards want on a read too.
async fn get_changes(addr: std::net::SocketAddr, id: &str) -> support::Response {
    let text = format!(
        "GET /api/sessions/{id}/changes HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    );
    request(addr, &text).await
}

#[tokio::test]
async fn the_endpoint_answers_the_eleven_keys() {
    let fixture = Fixture::new("changes-wire");
    fixture.commit("a.txt", b"a\n", "one file");
    let served = fixture.served().await;
    let response = get_changes(served.addr, &fixture.id).await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "the answer is not JSON"
    );
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    for key in [
        "branch",
        "base_commit",
        "head",
        "target",
        "ahead",
        "behind",
        "dirty",
        "mergeable",
        "files",
        "touches_ci",
        "mergeable_reason",
        "choices",
    ] {
        assert!(body.get(key).is_some(), "the answer has no {key}: {body}");
    }
    assert_eq!(body["branch"], Value::String(fixture.branch()));
    let files = body["files"].as_array().expect("files[] is an array");
    assert_eq!(files.len(), 1);
    for key in ["path", "status", "additions", "deletions"] {
        assert!(files[0].get(key).is_some(), "a file has no {key}");
    }
    assert_eq!(body["mergeable"], Value::Bool(true));
    assert_eq!(body["mergeable_reason"], Value::Null);
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn an_unknown_session_is_a_404() {
    let fixture = Fixture::new("changes-unknown");
    let served = fixture.served().await;
    let response = get_changes(served.addr, "0000000000000000").await;
    assert_eq!(response.status, 404, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("the envelope is JSON");
    assert_eq!(body["code"], "not_found");
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_session_with_no_worktree_is_the_storys_409() {
    // The stub backend provisions no repository at all, so the row has no `base_commit`. The answer is a
    // `409 no_worktree` with a sentence, never a 500 and never a subprocess. That the read involves no
    // container is **structural** rather than measured here: `Changes::read` takes a worktree, a commit and a
    // target list and has no backend to name, so this test would not notice a container if some other route
    // asked one. The measurement is the gate's `case_changes`, which runs the artifact with no daemon at all.
    let fixture = Fixture::new("changes-no-worktree");
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
                    client_key: Some("changes-no-worktree-2".to_string()),
                    base_commit: None,
                },
                clock::unix_now(),
            )
            .expect("a second session is created")
            .id
    };
    let served = fixture.served().await;
    let response = get_changes(served.addr, &fresh).await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body: Value = serde_json::from_str(&response.body).expect("the envelope is JSON");
    assert_eq!(body["code"], "no_worktree");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|sentence| sentence.contains("worktree")),
        "the sentence does not name the fact: {body}"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_missing_worktree_directory_is_a_409_and_not_a_500() {
    let fixture = Fixture::new("changes-wire-missing");
    std::fs::remove_dir_all(&fixture.worktree).expect("the worktree is removed");
    let served = fixture.served().await;
    let response = get_changes(served.addr, &fixture.id).await;
    assert_eq!(response.status, 409, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("the envelope is JSON");
    assert_eq!(body["code"], "no_worktree");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|sentence| sentence.contains("resume")),
        "the sentence names no next step: {body}"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_repository_git_cannot_read_is_a_500() {
    let fixture = Fixture::new("changes-wire-unreadable");
    std::fs::remove_dir_all(fixture.repo.join(".git"))
        .expect("the repository's directory is removed");
    std::fs::write(fixture.repo.join(".git"), b"gitdir: /nonexistent\n")
        .expect("the file is written");
    let served = fixture.served().await;
    let response = get_changes(served.addr, &fixture.id).await;
    assert_eq!(response.status, 500, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("the envelope is JSON");
    assert_eq!(body["code"], "internal");
    served.finish().await.expect("the server stops");
}

/// The 409 the handler gives a `ChangesError` arm, asserted at the wire — one shape for the three arms whose
/// module-level variant a test already proves. Mutating `refusal()` to fold any of them into `internal` used to
/// leave every test green (only the gate's pruned case caught one), which is the gap these close.
async fn the_wire_409_for(fixture: &Fixture, id: &str) -> String {
    let served = fixture.served().await;
    let response = get_changes(served.addr, id).await;
    assert_eq!(response.status, 409, "{} {}", fixture.id, response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    served.finish().await.expect("the server stops");
    response.body
}

#[tokio::test]
async fn a_pruned_worktree_is_a_409_at_the_wire() {
    let fixture = Fixture::new("changes-wire-pruned");
    git(
        &fixture.repo,
        &[
            "worktree",
            "remove",
            "--force",
            fixture.worktree.to_str().expect("a UTF-8 temp path"),
        ],
    );
    std::fs::create_dir_all(&fixture.worktree).expect("the directory is made again");
    let body = the_wire_409_for(&fixture, &fixture.id).await;
    let body: Value = serde_json::from_str(&body).expect("the envelope is JSON");
    assert_eq!(body["code"], "no_worktree");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|sentence| sentence.contains("registers")),
        "the sentence does not name the registration: {body}"
    );
}

#[tokio::test]
async fn a_missing_branch_is_a_409_at_the_wire() {
    let fixture = Fixture::new("changes-wire-no-branch");
    git(
        &fixture.repo,
        &[
            "worktree",
            "remove",
            "--force",
            fixture.worktree.to_str().expect("a UTF-8 temp path"),
        ],
    );
    git(&fixture.repo, &["branch", "-D", &fixture.branch()]);
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            fixture.worktree.to_str().expect("a UTF-8 temp path"),
        ],
    );
    let body = the_wire_409_for(&fixture, &fixture.id).await;
    let body: Value = serde_json::from_str(&body).expect("the envelope is JSON");
    assert_eq!(body["code"], "no_worktree");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|sentence| sentence.contains("branch")),
        "the sentence does not name the branch: {body}"
    );
}

#[tokio::test]
async fn a_missing_project_repository_is_a_409_at_the_wire() {
    let fixture = Fixture::new("changes-wire-no-repository");
    std::fs::remove_dir_all(&fixture.repo).expect("the repository is removed");
    let body = the_wire_409_for(&fixture, &fixture.id).await;
    let body: Value = serde_json::from_str(&body).expect("the envelope is JSON");
    assert_eq!(body["code"], "no_worktree");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|sentence| sentence.contains("restore")),
        "the sentence names no next step: {body}"
    );
}

#[tokio::test]
async fn a_repository_without_a_target_answers_json_null_not_a_missing_key() {
    // The acceptance's `target` is one key that can be null, and a client has to tell "there is no target" from
    // "the server did not send it". A `skip_serializing_if` on the `Option`s — or a `false` for an unasked
    // merge — would keep every module test green and break exactly this: `null` is an answer, absence is not.
    let fixture = Fixture::new("changes-wire-no-target");
    git(&fixture.repo, &["branch", "-m", "main", "trunk"]);
    let served = fixture.served().await;
    let response = get_changes(served.addr, &fixture.id).await;
    assert_eq!(response.status, 200, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    for key in ["target", "behind", "mergeable"] {
        assert!(
            body.get(key).is_some(),
            "{key} is absent rather than null: {body}"
        );
        assert_eq!(body[key], Value::Null, "{key} is not null: {body}");
    }
    assert!(
        body["mergeable_reason"].is_string(),
        "an unasked merge says why: {body}"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn the_route_is_read_only_and_reachable_with_the_ordinary_guards() {
    // A `GET` on the session prefix, so a request with no Origin and no client header is answered — the
    // guard rules are ADR-0010's and this route adds none. The manifest records it `ReadOnly`, which is what
    // keeps a read from ever being refused by a mutating route's rules.
    let fixture = Fixture::new("changes-guards");
    let served = fixture.served().await;
    let app = http::App::new(
        http::router(fixture.state.clone()),
        http::Guards::from_config(&fixture.state.config),
    );
    let entry = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}/changes")
        .expect("the route is in the manifest");
    assert_eq!(entry.kind, http::RouteKind::ReadOnly);
    assert_eq!(entry.method, Method::GET);
    let response = fetch(
        served.addr,
        &format!("/api/sessions/{}/changes", fixture.id),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    served.finish().await.expect("the server stops");
}

// --- the unified diff -----------------------------------------------------------------------------

/// The patch for one path, or `None` for the whole branch, as the handler would ask for it.
fn patch(fixture: &Fixture, path: Option<&str>) -> holosmith::changes::Diff {
    holosmith::changes::diff(&fixture.worktree(), &fixture.base, path).expect("the diff answers")
}

fn patch_err(fixture: &Fixture, path: Option<&str>) -> DiffError {
    holosmith::changes::diff(&fixture.worktree(), &fixture.base, path)
        .expect_err("the diff refuses")
}

/// git's own bytes for a command, **untrimmed** — `support::git` trims, and a patch is compared byte for byte.
///
/// The flags are the production ones, so what the test compares against is the same command the module runs
/// rather than a second reading of the same two commits.
fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=holosmith-test",
            "-c",
            "user.email=test@example",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    output.stdout
}

/// `git diff <base> <head> [-- <path>]` with the flags the module passes, as bytes.
fn git_patch(fixture: &Fixture, path: Option<&str>) -> Vec<u8> {
    let head = head(&fixture.worktree);
    let mut args = vec![
        "--literal-pathspecs",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--no-color",
        fixture.base.as_str(),
        head.as_str(),
    ];
    if let Some(path) = path {
        args.push("--");
        args.push(path);
    }
    git_bytes(&fixture.repo, &args)
}

/// Text a reader would call large: enough lines that the patch is comfortably past the cap.
fn large_text() -> Vec<u8> {
    let mut text = String::new();
    for line in 0..40_000 {
        text.push_str(&format!("+the {line}th line the agent wrote\n"));
    }
    text.into_bytes()
}

#[test]
fn the_patch_for_one_path_is_the_command_s_own_bytes() {
    // The acceptance's first bullet, measured rather than self-consistent: the body is compared with `git diff`
    // run by the test itself, so a wrong flag, a wrong range or a dropped hunk is a failure here.
    let fixture = Fixture::with_base("diff-one-file", &[("a.txt", b"a\nb\n")]);
    fixture.commit("a.txt", b"a\nB\nc\n", "the session's change");
    fixture.commit("other.txt", b"unrelated\n", "another file");
    let diff = patch(&fixture, Some("a.txt"));
    assert!(!diff.truncated);
    assert!(diff.utf8);
    assert_eq!(
        diff.body,
        git_patch(&fixture, Some("a.txt")),
        "the body is not git's own diff of a.txt"
    );
    assert!(diff.total > 0);
}

#[test]
fn the_whole_branch_patch_names_every_changed_file() {
    let fixture = Fixture::with_base("diff-branch", &[("a.txt", b"a\n")]);
    fixture.commit("a.txt", b"a\nB\n", "one");
    fixture.commit("b.txt", b"new\n", "two");
    let diff = patch(&fixture, None);
    let text = String::from_utf8(diff.body.clone()).expect("the patch is UTF-8");
    for path in ["a.txt", "b.txt"] {
        assert!(
            text.contains(&format!("diff --git a/{path} b/{path}")),
            "the branch patch does not name {path}: {text}"
        );
    }
    assert_eq!(diff.body, git_patch(&fixture, None));
}

#[test]
fn a_binary_file_is_summarised_and_never_emitted() {
    // The acceptance's fourth bullet. git's own line *is* the summary, so this compares against it byte for
    // byte and then asserts the thing the bullet really asks: no NUL byte reaches the body.
    let fixture = Fixture::with_base("diff-binary", &[("bin.dat", b"before\n")]);
    fixture.commit("bin.dat", BINARY, "the binary changes");
    let diff = patch(&fixture, Some("bin.dat"));
    let text = String::from_utf8(diff.body.clone()).expect("a summary is text");
    assert!(
        text.contains("Binary files a/bin.dat and b/bin.dat differ"),
        "the body is not a summary: {text}"
    );
    assert!(!diff.body.contains(&0), "a NUL byte reached the body");
    assert_eq!(diff.body, git_patch(&fixture, Some("bin.dat")));
}

#[test]
fn an_over_cap_patch_is_cut_with_a_marker_inside_the_cap() {
    let fixture = Fixture::new("diff-cap");
    fixture.commit("big.txt", &large_text(), "the large file");
    let whole = git_patch(&fixture, Some("big.txt"));
    assert!(
        whole.len() > DIFF_CAP,
        "the fixture is not past the cap: {} bytes",
        whole.len()
    );
    let diff = patch(&fixture, Some("big.txt"));
    assert!(diff.truncated);
    assert_eq!(
        diff.total as usize,
        whole.len(),
        "the marker's total is not git's own byte count"
    );
    assert!(
        diff.body.len() <= DIFF_CAP,
        "the response is {} bytes, past the cap",
        diff.body.len()
    );
    let text = String::from_utf8(diff.body.clone()).expect("the kept prefix is UTF-8");
    let marker = text.lines().last().expect("a last line");
    assert!(marker.starts_with("[diff truncated:"), "{marker}");
    assert!(marker.contains(&whole.len().to_string()), "{marker}");
    let kept = &text[..text.len() - marker.len() - 1];
    assert!(kept.ends_with('\n'), "the cut is not a line boundary");
    assert!(
        whole.starts_with(kept.as_bytes()),
        "the kept text is not a prefix of git's own patch"
    );
}

/// An executable script in the projects root, for the tests that make a repository's config name a program.
///
/// Written outside the worktree on purpose: a helper file inside it would be untracked work, and these tests are
/// about what git *runs*, not about what it sees.
fn script(fixture: &Fixture, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = fixture.projects.join(name);
    std::fs::write(&path, body).expect("the script is written");
    let mut mode = std::fs::metadata(&path)
        .expect("the script is a file")
        .permissions();
    mode.set_mode(0o755);
    std::fs::set_permissions(&path, mode).expect("the script is executable");
    path
}

#[test]
fn a_repository_config_cannot_run_a_program_for_the_patch() {
    // `--no-ext-diff`, measured rather than pinned: `diff.external` in the repository's own config *is* honoured
    // by a plain `git diff` — the script below replaces the whole answer with one line — so removing the flag
    // turns this test red. The vector test pins the spelling; this one measures what the spelling buys.
    let fixture = Fixture::with_base("diff-ext", &[("a.txt", b"a\n")]);
    fixture.commit("a.txt", b"a\nB\n", "the session's change");
    let program = script(
        &fixture,
        "external-diff.sh",
        "#!/bin/sh\necho 'EXTERNAL DIFF RAN'\n",
    );
    git(
        &fixture.repo,
        &[
            "config",
            "diff.external",
            program.to_str().expect("a UTF-8 path"),
        ],
    );
    let diff = patch(&fixture, Some("a.txt"));
    let text = String::from_utf8(diff.body.clone()).expect("the patch is UTF-8");
    assert!(
        !text.contains("EXTERNAL DIFF RAN"),
        "a repository config ran a program for the patch: {text}"
    );
    assert_eq!(diff.body, git_patch(&fixture, Some("a.txt")));
}

#[test]
fn a_repository_config_cannot_make_a_binary_file_look_textual() {
    // `--no-textconv`, measured: a `.gitattributes` driver with a shell textconv turns the binary file's summary
    // into a diff of whatever that program prints. Without the flag git runs it and the acceptance's "binary
    // files are summarised, not emitted" is false for a repository that says so.
    let fixture = Fixture::with_base(
        "diff-textconv",
        &[
            ("bin.dat", b"\x00before\n"),
            (".gitattributes", b"*.dat diff=mine\n"),
        ],
    );
    fixture.commit("bin.dat", BINARY, "the binary changes");
    let program = script(&fixture, "textconv.sh", "#!/bin/sh\ncat \"$1\"\n");
    git(
        &fixture.repo,
        &[
            "config",
            "diff.mine.textconv",
            program.to_str().expect("a UTF-8 path"),
        ],
    );
    let diff = patch(&fixture, Some("bin.dat"));
    assert!(
        diff.body
            .windows(12)
            .any(|window| window == b"Binary files"),
        "the textconv filter was run: {}",
        String::from_utf8_lossy(&diff.body)
    );
    assert!(!diff.body.contains(&0), "a NUL byte reached the body");
}

#[test]
fn a_repository_config_cannot_colour_the_patch() {
    // `--no-color`, measured: `color.ui = always` in a repository's config makes a *piped* diff carry ANSI
    // escapes, which is not text a phone can read. It is the same class of fact as the two above — a read that
    // inherits config is not a measurement — and it is why the flag is in the vector at all.
    let fixture = Fixture::with_base("diff-colour", &[("a.txt", b"a\n")]);
    fixture.commit("a.txt", b"a\nB\n", "the session's change");
    git(&fixture.repo, &["config", "color.ui", "always"]);
    let diff = patch(&fixture, Some("a.txt"));
    assert!(
        !diff.body.contains(&0x1b),
        "an escape reached the body: {:?}",
        String::from_utf8_lossy(&diff.body)
    );
    assert_eq!(diff.body, git_patch(&fixture, Some("a.txt")));
}

#[test]
fn a_path_that_did_not_change_is_refused_without_asking_git() {
    // The acceptance's fifth bullet. Three spellings of a miss and one of them is the sharp case: `sub/../a.txt`
    // *normalises* to a changed file, so an implementation that resolved the path instead of comparing it would
    // answer a patch. The list is the answer — git is never asked about a name it did not print.
    let fixture = Fixture::with_base(
        "diff-miss",
        &[("a.txt", b"a\n"), ("keep.txt", b"unchanged\n")],
    );
    fixture.commit("a.txt", b"a\nB\n", "the session's change");
    for path in [
        "keep.txt",
        "sub/../a.txt",
        "../etc/passwd",
        "/etc/passwd",
        "",
    ] {
        assert_eq!(
            patch_err(&fixture, Some(path)),
            DiffError::NotChanged {
                path: path.to_string()
            },
            "{path} was not refused"
        );
    }
    // And the file it names is really changed, so the refusal above is the membership test and not "there is
    // nothing to show": git answers a patch when asked directly.
    assert!(!git_patch(&fixture, Some("a.txt")).is_empty());
}

#[test]
fn an_empty_branch_is_an_empty_patch_rather_than_an_error() {
    // D6's second half: "nothing changed yet" is an answer, not a refusal and not a `500`. The fixture is a fresh
    // session with no commit on `agent/<id>`, which is what a create that never ran a turn looks like.
    let fixture = Fixture::new("diff-empty");
    let diff = patch(&fixture, None);
    assert!(diff.body.is_empty(), "an empty branch has nothing to say");
    assert!(!diff.truncated);
    assert_eq!(diff.total, 0);
    assert!(diff.utf8);
}

#[test]
fn a_name_that_looks_like_a_pattern_is_the_name_and_not_a_pattern() {
    // A git pathspec is a glob by default, and `files[]` is a list of exact names. A changed file literally called
    // `a*.md` must be handed to git as itself: without `--literal-pathspecs` the answer for `?path=a*.md` would
    // also carry `abc.md`, which is a diff of a file the client did not ask for.
    let fixture = Fixture::new("diff-glob");
    fixture.commit("a*.md", b"star\n", "one");
    fixture.commit("abc.md", b"abc\n", "two");
    let diff = patch(&fixture, Some("a*.md"));
    let text = String::from_utf8(diff.body.clone()).expect("the patch is UTF-8");
    assert!(text.contains("diff --git a/a*.md b/a*.md"), "{text}");
    assert!(
        !text.contains("abc.md"),
        "the name was expanded as a pattern: {text}"
    );
}

#[test]
fn a_worktree_that_is_gone_is_the_summary_s_refusal_not_a_500() {
    // The diff inherits the summary's ordered refusals, so a session with no worktree answers the same fact
    // here — never a `500` from a git that was asked about a directory that is not there.
    let fixture = Fixture::new("diff-gone");
    std::fs::remove_dir_all(&fixture.worktree).expect("the worktree is removed");
    match patch_err(&fixture, Some("a.txt")) {
        DiffError::Changes(ChangesError::Missing { .. }) => {}
        other => panic!("a missing worktree is not the summary's refusal: {other:?}"),
    }
}

#[test]
fn the_result_does_not_depend_on_the_working_tree() {
    // Commits only: an uncommitted change in the worktree is `dirty`'s fact on the summary, and it must not
    // appear in the patch — otherwise "the diff of this branch" would move between two identical requests.
    let fixture = Fixture::with_base("diff-dirty", &[("a.txt", b"a\n")]);
    fixture.commit("a.txt", b"a\nB\n", "committed");
    let committed = patch(&fixture, Some("a.txt"));
    std::fs::write(fixture.worktree.join("a.txt"), b"a\nuncommitted\n")
        .expect("the file is written");
    assert_eq!(patch(&fixture, Some("a.txt")).body, committed.body);
}

// --- the diff at the wire ----------------------------------------------------------------------------------

/// A GET of one path with an optional query, the way the app's own client sends it.
async fn get_diff(addr: std::net::SocketAddr, id: &str, query: &str) -> support::Response {
    fetch(addr, &format!("/api/sessions/{id}/changes/diff{query}")).await
}

#[tokio::test]
async fn the_diff_endpoint_answers_a_patch_as_text() {
    let fixture = Fixture::with_base("diff-wire", &[("a.txt", b"a\n")]);
    fixture.commit("a.txt", b"a\nB\n", "the session's change");
    let served = fixture.served().await;
    let response = get_diff(served.addr, &fixture.id, "?path=a.txt").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("text/plain; charset=utf-8"),
        "a patch is not JSON and not an octet stream"
    );
    assert_eq!(response.bytes, git_patch(&fixture, Some("a.txt")));
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn no_path_answers_the_whole_branch_at_the_wire() {
    let fixture = Fixture::with_base("diff-wire-branch", &[("a.txt", b"a\n")]);
    fixture.commit("a.txt", b"a\nB\n", "one");
    fixture.commit("b.txt", b"new\n", "two");
    let served = fixture.served().await;
    let response = get_diff(served.addr, &fixture.id, "").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.bytes, git_patch(&fixture, None));
    for path in ["a.txt", "b.txt"] {
        assert!(
            response
                .body
                .contains(&format!("diff --git a/{path} b/{path}")),
            "the answer does not name {path}: {}",
            response.body
        );
    }
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_binary_file_answers_a_summary_and_no_nul_byte_at_the_wire() {
    let fixture = Fixture::with_base("diff-wire-binary", &[("bin.dat", b"before\n")]);
    fixture.commit("bin.dat", BINARY, "the binary changes");
    let served = fixture.served().await;
    let response = get_diff(served.addr, &fixture.id, "?path=bin.dat").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        response
            .body
            .contains("Binary files a/bin.dat and b/bin.dat differ"),
        "the body is not a summary: {}",
        response.body
    );
    assert!(!response.bytes.contains(&0), "a NUL byte reached the wire");
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn an_over_cap_patch_is_capped_at_the_wire() {
    let fixture = Fixture::new("diff-wire-cap");
    fixture.commit("big.txt", &large_text(), "the large file");
    let served = fixture.served().await;
    let response = get_diff(served.addr, &fixture.id, "?path=big.txt").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        response.bytes.len() <= DIFF_CAP,
        "the answer is {} bytes, past the cap",
        response.bytes.len()
    );
    assert!(
        response
            .body
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("[diff truncated:")),
        "the last line is not the marker: {}",
        response.body.lines().last().unwrap_or_default()
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_traversal_and_an_unchanged_path_are_the_same_404_at_the_wire() {
    // The encoding is the client's, so the handler decodes before comparing: `..%2Fetc%2Fpasswd` is the literal
    // string `../etc/passwd`, which no changed path is. One answer for both because the client's move is one:
    // pick a path out of `files[]`.
    let fixture = Fixture::with_base("diff-wire-miss", &[("a.txt", b"a\n"), ("keep.txt", b"x\n")]);
    fixture.commit("a.txt", b"a\nB\n", "the session's change");
    let served = fixture.served().await;
    for query in [
        "?path=..%2Fetc%2Fpasswd",
        "?path=%2Fetc%2Fpasswd",
        "?path=keep.txt",
        "?path=",
    ] {
        let response = get_diff(served.addr, &fixture.id, query).await;
        assert_eq!(response.status, 404, "{query}: {}", response.body);
        assert_eq!(response.header("content-type"), Some("application/json"));
        let body: Value = serde_json::from_str(&response.body).expect("the envelope is JSON");
        assert_eq!(body["code"], "not_found", "{query}: {body}");
        assert!(
            !response.body.contains("root:"),
            "{query}: the answer leaked a file"
        );
    }
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_percent_encoded_name_arrives_as_the_name_git_printed() {
    // A path with a space is the one case where decoding is the difference between answering and refusing: git
    // printed `two words.txt` and a client sends `two%20words.txt`.
    let fixture = Fixture::new("diff-wire-encoded");
    fixture.commit("two words.txt", b"one\n", "a name with a space");
    let served = fixture.served().await;
    let response = get_diff(served.addr, &fixture.id, "?path=two%20words.txt").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        response
            .body
            .contains("diff --git a/two words.txt b/two words.txt")
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn a_session_with_no_worktree_is_the_summary_s_409_here_too() {
    // The same fact and the same sentence as `GET …/changes`: a client that learned one route learned both.
    let fixture = Fixture::new("diff-wire-no-worktree");
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
                    client_key: Some("changes-diff-no-worktree".to_string()),
                    base_commit: None,
                },
                clock::unix_now(),
            )
            .expect("a second session is created")
            .id
    };
    let served = fixture.served().await;
    let response = get_diff(served.addr, &fresh, "?path=a.txt").await;
    assert_eq!(response.status, 409, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("the envelope is JSON");
    assert_eq!(body["code"], "no_worktree");
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn the_diff_route_is_read_only_and_in_the_manifest() {
    // A `GET`, so the manifest records `ReadOnly` and every guard applies with nothing new — the property that
    // keeps a read from ever being refused by a mutating route's rules.
    let fixture = Fixture::new("diff-guards");
    let app = http::App::new(
        http::router(fixture.state.clone()),
        http::Guards::from_config(&fixture.state.config),
    );
    let entry = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}/changes/diff")
        .expect("the route is in the manifest");
    assert_eq!(entry.kind, http::RouteKind::ReadOnly);
    assert_eq!(entry.method, Method::GET);
}
