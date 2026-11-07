//! The changes view's summary over a real repository (HS-052, spec item 12).
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
use holosmith::changes::{Changes, ChangesError, touches_ci};
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
        Changes::read(&self.worktree(), &self.base, targets).expect("the changes view answers")
    }

    fn read_err(&self) -> ChangesError {
        Changes::read(
            &self.worktree(),
            &self.base,
            &["main".to_string(), "master".to_string()],
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
    // The P1 lesson of HS-050's review, as a test: no target is "I could not ask", and folding it into
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
    // `409 no_worktree` with a sentence, never a 500 and never a subprocess — which is also the measurement
    // for "no container is involved": this handler never asks the backend anything.
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
