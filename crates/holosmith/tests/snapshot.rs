//! The WIP snapshot (HS-042): one ref, a temporary index, and a branch that does not move.

use std::path::{Path, PathBuf};
use std::process::Command;

use holosmith::snapshot::{self, Taken};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("holosmith-snapshot-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the temp directory is creatable");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn git(path: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn repository(directory: &TempDir) -> PathBuf {
    let project = directory.path.join("project");
    std::fs::create_dir_all(&project).expect("the project is creatable");
    git(&project, &["init", "--quiet", "-b", "main", "."]);
    git(&project, &["config", "user.name", "test"]);
    git(&project, &["config", "user.email", "test@example"]);
    std::fs::write(project.join("tracked.txt"), "base\n").expect("the file is writable");
    git(&project, &["add", "tracked.txt"]);
    git(&project, &["commit", "--quiet", "-m", "base"]);
    project
}

fn worktree(directory: &TempDir, id: &str) -> (PathBuf, PathBuf) {
    let project = repository(directory);
    let path = directory.path.join("worktree");
    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            &format!("agent/{id}"),
            path.to_str().expect("UTF-8"),
            "HEAD",
        ],
    );
    (project, path)
}

#[test]
fn a_snapshot_keeps_uncommitted_work_without_moving_the_branch_or_the_index() {
    let directory = TempDir::new("keep");
    let id = "0123456789abcdef";
    let (project, path) = worktree(&directory, id);
    let head = git(&path, &["rev-parse", "HEAD"]);
    std::fs::write(path.join("tracked.txt"), "changed but not committed\n").expect("write");
    std::fs::write(path.join("new.txt"), "new file\n").expect("write");
    let taken = snapshot::take(&path, &directory.path.join("wip.index"), id).expect("snapshot");
    let Taken::Written { commit, .. } = taken else {
        panic!("a changed tree must write a snapshot: {taken:?}");
    };
    assert_eq!(
        git(&project, &["rev-parse", &snapshot::reference(id)]),
        commit,
        "the ref points at the snapshot commit"
    );
    assert_eq!(
        git(
            &project,
            &["show", &format!("{}:tracked.txt", snapshot::reference(id))]
        ),
        "changed but not committed"
    );
    assert_eq!(
        git(
            &project,
            &["show", &format!("{}:new.txt", snapshot::reference(id))]
        ),
        "new file"
    );
    // The branch and the worktree's index are untouched: the change is still unstaged, and HEAD is still
    // where it was.
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&path, &["diff", "--cached", "--name-only"]), "");
    assert!(git(&path, &["status", "--porcelain"]).contains("tracked.txt"));
    assert!(
        !directory.path.join("wip.index").exists(),
        "the temporary index is removed"
    );
}

#[test]
fn a_tree_equal_to_head_removes_the_ref_instead_of_writing_another_commit() {
    let directory = TempDir::new("clean");
    let id = "fedcba9876543210";
    let (project, path) = worktree(&directory, id);
    std::fs::write(path.join("tracked.txt"), "change\n").expect("write");
    let Taken::Written { commit, .. } =
        snapshot::take(&path, &directory.path.join("wip.index"), id).expect("snapshot")
    else {
        panic!("the first snapshot writes");
    };
    assert_eq!(
        git(&project, &["rev-parse", &snapshot::reference(id)]),
        commit
    );
    // Put the worktree back to HEAD's tree and take another snapshot: nothing to keep, so the ref goes.
    std::fs::write(path.join("tracked.txt"), "base\n").expect("write");
    assert_eq!(
        snapshot::take(&path, &directory.path.join("wip.index"), id).expect("snapshot"),
        Taken::Removed
    );
    let reference = snapshot::reference(id);
    let answer = Command::new("git")
        .arg("-C")
        .arg(&project)
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .status()
        .expect("git runs");
    assert!(!answer.success(), "the ref was removed");
}

#[test]
fn a_worktree_that_is_not_there_is_a_sentence_and_not_a_commit() {
    let directory = TempDir::new("missing");
    let error = snapshot::take(
        &directory.path.join("no-worktree"),
        &directory.path.join("wip.index"),
        "0123456789abcdef",
    )
    .expect_err("there is nothing to snapshot");
    assert!(
        error.to_string().contains("there is no worktree at"),
        "{error}"
    );
}
