//! The WIP snapshot: uncommitted work saved without touching the branch or the working tree (HS-042,
//! spec item 9).
//!
//! Spec item 9's script is four git commands over a **temporary index**:
//!
//! ```text
//! GIT_INDEX_FILE=<tmp> git read-tree HEAD
//! GIT_INDEX_FILE=<tmp> git add -A
//! tree=$(GIT_INDEX_FILE=<tmp> git write-tree)
//! commit=$(git -c user.name=holosmith -c user.email=holosmith@localhost \
//!            commit-tree "$tree" -p HEAD -m 'holosmith snapshot')
//! git update-ref refs/holosmith/wip/<id> "$commit"
//! ```
//!
//! The rule the index file makes true is the acceptance's third bullet: the session's index, its working
//! tree and its branch are never modified. `git add` is the only command that would touch the index, and
//! `GIT_INDEX_FILE` sends it to a file this module creates and removes. `read-tree HEAD` seeds that file
//! with the commit, so a tracked file that the agent deleted is staged as a deletion rather than ignored.
//!
//! **When the tree equals HEAD's tree the ref is deleted instead of written.** That is "nothing to save"
//! in one shape rather than a ref pointing at a commit identical to the branch's HEAD: HS-043's keep-rule
//! is "the wip ref differs from the branch", and a ref that does not differ is noise.
//!
//! Synchronous and `std` only, like [`crate::worktree`]: one program, one answer. The Docker backend runs
//! it from `spawn_blocking`, and `holosmith container snapshot` runs it in a container's `/work`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The ref namespace a session's snapshot lives under.
pub const REF_PREFIX: &str = "refs/holosmith/wip/";

/// The commit message spec item 9 spells.
pub const MESSAGE: &str = "holosmith snapshot";

/// The identity the snapshot commit carries. `-c` on the command, never a global config write.
pub const IDENTITY: [(&str, &str); 2] = [
    ("user.name", "holosmith"),
    ("user.email", "holosmith@localhost"),
];

/// What one snapshot did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Taken {
    /// A commit was written and the ref points at it.
    Written { commit: String, tree: String },
    /// The worktree's tree equals HEAD's tree, so the ref was removed (or never existed).
    Removed,
}

impl Taken {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Written { .. } => "written",
            Self::Removed => "removed",
        }
    }
}

/// Why a snapshot could not be taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// The worktree directory is not there.
    NotAWorktree(PathBuf),
    /// The temporary index could not be made, which is a filesystem fact rather than a git one.
    Index { path: PathBuf, detail: String },
    /// A git command could not start, or answered non-zero. The text is git's own last words.
    Git { command: String, detail: String },
}

impl SnapshotError {
    fn git(worktree: &Path, args: &[&str], detail: impl Into<String>) -> Self {
        Self::Git {
            command: format!("git -C {} {}", worktree.display(), args.join(" ")),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAWorktree(path) => {
                write!(f, "there is no worktree at {} to snapshot", path.display())
            }
            Self::Index { path, detail } => write!(
                f,
                "the temporary snapshot index {} could not be made: {detail}",
                path.display()
            ),
            Self::Git { command, detail } => write!(f, "{command}: {detail}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// Take the snapshot of `worktree` for session `id`, using `index` as the temporary index.
///
/// The index's parent is created, and the file is removed whatever happens. Every git command that reads
/// or writes the index carries `GIT_INDEX_FILE=<index>`, which is what keeps the session's own index out
/// of it.
pub fn take(worktree: &Path, index: &Path, id: &str) -> Result<Taken, SnapshotError> {
    if !worktree.is_dir() {
        return Err(SnapshotError::NotAWorktree(worktree.to_path_buf()));
    }
    if let Some(parent) = index.parent() {
        std::fs::create_dir_all(parent).map_err(|error| SnapshotError::Index {
            path: index.to_path_buf(),
            detail: error.to_string(),
        })?;
    }
    let _ = std::fs::remove_file(index);
    let result = take_inner(worktree, index, id);
    let _ = std::fs::remove_file(index);
    result
}

fn take_inner(worktree: &Path, index: &Path, id: &str) -> Result<Taken, SnapshotError> {
    let env = [("GIT_INDEX_FILE", index)];
    run(worktree, &["read-tree", "HEAD"], &env)?;
    run(worktree, &["add", "-A"], &env)?;
    let tree = run(worktree, &["write-tree"], &env)?;
    let head_tree = run(worktree, &["rev-parse", "HEAD^{tree}"], &[])?;
    let reference = reference(id);
    if tree == head_tree {
        if run(
            worktree,
            &["rev-parse", "--verify", "--quiet", &reference],
            &[],
        )
        .is_ok()
        {
            run(worktree, &["update-ref", "-d", &reference], &[])?;
        }
        return Ok(Taken::Removed);
    }
    let name = format!("{}={}", IDENTITY[0].0, IDENTITY[0].1);
    let email = format!("{}={}", IDENTITY[1].0, IDENTITY[1].1);
    let commit = run(
        worktree,
        &[
            "-c",
            &name,
            "-c",
            &email,
            "commit-tree",
            &tree,
            "-p",
            "HEAD",
            "-m",
            MESSAGE,
        ],
        &[],
    )?;
    run(worktree, &["update-ref", &reference, &commit], &[])?;
    Ok(Taken::Written { commit, tree })
}

/// Run one git command in `worktree` and answer its trimmed stdout; a non-zero exit is an error.
fn run(
    worktree: &Path,
    args: &[&str],
    environment: &[(&str, &Path)],
) -> Result<String, SnapshotError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(worktree).args(args);
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().map_err(|error| {
        SnapshotError::git(
            worktree,
            args,
            format!("git is not runnable ({error}): install git 2.38 or newer"),
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(SnapshotError::git(
            worktree,
            args,
            if detail.is_empty() {
                format!("git exited {} without saying why", output.status)
            } else {
                detail.to_string()
            },
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The ref a session's snapshot lives at. Used by the delete flow's recovery sentence (HS-043).
pub fn reference(id: &str) -> String {
    format!("{REF_PREFIX}{id}")
}
