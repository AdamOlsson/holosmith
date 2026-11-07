//! The merge: a session's branch landed on a target branch (HS-054, spec item 12's rules 2 and 4).
//!
//! Item 12's rules, in full, are what this module answers — but only two of the five are this story's:
//!
//! ```text
//! Rule 2. One merge per project at a time. … `ff-only` skips the temporary worktree and fast-forwards
//!         directly, failing with 409 `not_fast_forward` if the target has moved.
//! Rule 4. Updating the target: if it is not checked out anywhere, `git update-ref refs/heads/<target>
//!         <new> <old>` (compare-and-swap). If it is checked out in the main checkout, `git merge
//!         --ff-only <new>` runs there, which updates the working tree safely and fails with 409
//!         `target_dirty` if your local changes would be overwritten.
//! ```
//!
//! **This is the first module in the build that writes to the project repository.** Everything before it
//! read: the changes view's summary, its unified diff, the worktree's refusals, `doctor`'s merge trial. So the
//! module's shape is the one a write needs rather than a read's:
//!
//! - **One merge per project at a time** ([`Merges`]): a `std::sync::Mutex` per repository path, held for the
//!   whole write. It is taken inside the caller's `spawn_blocking`, because that is where the whole operation
//!   runs and a `std` mutex across an `.await` would park a runtime worker.
//! - **The four refusals are the changes view's** ([`crate::changes::basis`]): "there is no worktree" is one
//!   fact with four sentences, and a client that learned `GET …/changes` learned the merge's refusals. The
//!   module imports that function rather than copying it; what it *adds* is the target's own three facts.
//! - **Every answer is measured, never inferred from git's English.** `merge-base --is-ancestor` exits 0/1 as
//!   an answer (the same shape HS-043's keep-rule reads), `update-ref`'s compare-and-swap is the race-free
//!   guard, and a failed `git merge --ff-only` is classified by re-measuring two facts of our own — the
//!   ancestry and the checkout's status — rather than by looking for a sentence in git's stderr.
//! - **Hooks never run**: every `git merge` this module runs carries `-c core.hooksPath=/dev/null`, item 12's
//!   own sentence ("so that your hooks never run agent-written code"), which rule 2 spells for the temporary
//!   worktree and which is just as true of a fast-forward in the operator's own checkout (`post-merge` is the
//!   hook a fast-forward can run).
//!
//! What this module does not do: no `merge` or `squash` strategy and no temporary worktree (HS-055), no
//! conflict arm, no `delete_session` and none of rule 1's guards (HS-056), no publish (HS-057). It also does
//! not touch the database: a merge is neither a state transition nor an event, and `GET …/changes` answers
//! `ahead: 0` afterwards because that is a measurement of the branch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;

use crate::changes::{ChangesError, basis, dirty};
use crate::worktree::{
    Checkout, GitError, Worktree, checkouts, git_command, git_error, git_output,
};

/// The one strategy this build has: item 12's default, and the only one that needs no temporary worktree.
///
/// A constant rather than a literal in two places, because it is a word on the wire (the response's
/// `strategy`), a word in the request (an explicit `"ff-only"` is the default spelled out) and a word in the
/// `unsupported` refusals. `merge` and `squash` arrive with HS-055 and will become a `Strategy` type then;
/// spending one on a one-variant enum now would be a shape with nothing to distinguish.
pub const FF_ONLY: &str = "ff-only";

/// The one `git merge` line this module runs, and the reason it is a function rather than a literal.
///
/// `-c core.hooksPath=/dev/null` is item 12's rule 2 spelled for the temporary worktree and applied here for
/// the same reason: a `post-merge` hook in a project repository is a program the host runs, and after an agent
/// has been writing into `/work` the operator's hooks are not code this build is willing to execute on its
/// behalf. `--ff-only` is the strategy's whole meaning: there is no merge commit, so a target that has moved
/// past the branch is a refusal rather than a three-way merge.
///
/// The argument order is the decision: options before the subcommand (`-c` is a git-wide option, not `merge`'s),
/// and the commit last.
fn merge_arguments(new: &str) -> Vec<String> {
    [
        "-c".to_string(),
        "core.hooksPath=/dev/null".to_string(),
        "merge".to_string(),
        "--ff-only".to_string(),
        new.to_string(),
    ]
    .to_vec()
}

/// The compare-and-swap: `git update-ref <ref> <new> <old>`, three arguments in that order.
///
/// The third argument *is* the race-free half of rule 4 — git refuses the write unless the ref still holds
/// `<old>`, so a target that moved between this module's read and its write is caught by git rather than by a
/// window this build cannot close. Dropping it would make the update a blind write, which is why it has its own
/// test rather than being one line inside [`merge`].
fn update_ref_arguments(reference: &str, new: &str, old: &str) -> Vec<String> {
    [
        "update-ref".to_string(),
        reference.to_string(),
        new.to_string(),
        old.to_string(),
    ]
    .to_vec()
}

/// What a merge did: the three keys spec item 12's rule 6 names, and nothing else.
///
/// `merged_commit` is the target's commit **after** the merge — the answer to "where did the work land" — and
/// for a branch that was already merged it is the same commit the target was at, because a fast-forward of an
/// ancestor is a no-op rather than an error. `strategy` is the strategy that ran, not the one that was asked
/// for: this build refuses the two it cannot run rather than echoing them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Merged {
    /// The commit the target now points at, which is the session branch's tip.
    pub merged_commit: String,
    /// The branch that moved.
    pub target: String,
    /// [`FF_ONLY`], the strategy that ran.
    pub strategy: String,
}

/// One merge per project at a time: rule 2's serialisation, and the only thing in this module that is state.
///
/// A mutex **per repository path** rather than one for the whole machine: the spec's rule is per project, and
/// two projects' merges are two repositories that cannot interfere. The map is created lazily and never
/// cleaned, which bounds it by the number of projects a machine has — a number an operator chooses once, not
/// one that grows with traffic.
///
/// A [`std::sync::Mutex`] rather than a `tokio` one because the whole merge runs on a blocking task: there is
/// no `.await` inside the scope, and a `std` mutex is honest exactly where that is true.
#[derive(Debug, Clone, Default)]
pub struct Merges {
    held: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>,
}

impl Merges {
    /// An empty set of project locks.
    pub fn new() -> Self {
        Self::default()
    }

    /// The lock for one project's repository, made on first use.
    ///
    /// The outer mutex is held only long enough to clone one `Arc`, so the map is never a bottleneck; the
    /// inner one is what a merge holds. A poisoned mutex is recovered rather than panicked on: the project's
    /// next merge must still be able to run, and the state a panic left behind is the filesystem's, not this
    /// map's.
    fn project(&self, repository: &Path) -> Arc<Mutex<()>> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(held.entry(repository.to_path_buf()).or_default())
    }
}

/// Why a merge did not happen — one variant per fact an operator can act on.
///
/// The first arm is the changes view's whole family ([`ChangesError`]), because "there is no worktree to
/// merge" is that module's question and its five sentences are already on the wire. Everything below it is
/// this module's: two 422s (the request named a target this machine will not merge into), two 409s (a state
/// of the repository the operator has to change), and one 500 (git failed).
///
/// The variants carry the names and paths the *log* is allowed to mention. The envelope's messages are fixed
/// sentences (ADR-0008), so a path a sentence would have carried goes to the log instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeError {
    /// Nothing to merge, or git could not be asked — [`ChangesError`]'s own two families.
    Changes(ChangesError),
    /// `target` is not one of the configured `[merge] targets`: the parameter is not one this machine accepts.
    Unlisted {
        target: String,
        targets: Vec<String>,
    },
    /// `target` is configured, and the repository has no such branch: still the parameter's fault, since
    /// `[merge] targets` naming a branch a project does not have is the ordinary case (a machine has `main`
    /// or `master`, and the default list names both).
    NoTarget { target: String },
    /// The target has commits the session's branch does not — or it moved between this module's read and its
    /// write, which is the same fact observed a moment later.
    NotFastForward { target: String, branch: String },
    /// The checkout that holds the target has local work the fast-forward would overwrite.
    TargetDirty { target: String, path: PathBuf },
    /// The target is checked out somewhere that is not the project's own checkout — in practice a session's
    /// worktree, where a container is working.
    CheckedOutElsewhere { target: String, path: PathBuf },
}

impl From<ChangesError> for MergeError {
    fn from(error: ChangesError) -> Self {
        Self::Changes(error)
    }
}

impl From<GitError> for MergeError {
    fn from(error: GitError) -> Self {
        Self::Changes(ChangesError::Git(error))
    }
}

impl MergeError {
    /// The sentence an operator reads in the log, with the path the envelope is not allowed to carry.
    pub fn sentence(&self) -> String {
        match self {
            Self::Changes(error) => error.sentence(),
            Self::Unlisted { target, targets } => {
                format!(
                    "the target {target} is not one of the configured merge targets [{}]",
                    targets.join(", ")
                )
            }
            Self::NoTarget { target } => {
                format!("this repository has no branch {target}")
            }
            Self::NotFastForward { target, branch } => {
                format!("{target} has commits {branch} does not, so it cannot be fast-forwarded")
            }
            Self::TargetDirty { target, path } => format!(
                "the checkout {} holds local work the fast-forward of {target} would overwrite",
                path.display()
            ),
            Self::CheckedOutElsewhere { target, path } => format!(
                "{target} is checked out in {}, which is not the project's own checkout",
                path.display()
            ),
        }
    }
}

/// Fast-forward a session's branch into one of the configured targets, and answer where the work landed.
///
/// The order of the questions is the order a refusal is cheapest and most fundamental first, and the first two
/// are the *request's* rather than the session's:
///
/// 1. **the target is one the config names** — pure, no repository, and a request that names a branch this
///    machine will not merge into is wrong whatever the session's state is ([`MergeError::Unlisted`]);
/// 2. **then one merge per project at a time** — the lock is taken before the first git read, because the
///    whole operation is a write and a second merge that read *before* the first wrote would decide on a state
///    that is no longer there;
/// 3. **the four `no_worktree` facts and the branch's tip** — [`basis`], the changes view's own function;
/// 4. **the target is a branch this repository has** ([`MergeError::NoTarget`]);
/// 5. **the target is an ancestor of the branch** — the fast-forward test, and the honest one: a target with
///    commits the branch lacks cannot be fast-forwarded at all;
/// 6. **where the target is checked out** — the repository's own checkout, nowhere, or somewhere else;
/// 7. **the write**, by one of rule 4's two commands.
///
/// A session row is *not* consulted and *not* written: this function knows a worktree, a target and the config's
/// list. Whether the session may be merged at all (a running turn, uncommitted work) is HS-056's rule 1, and
/// this story deliberately does not pre-empt it.
pub fn merge(
    worktree: &Worktree,
    target: &str,
    targets: &[String],
    merges: &Merges,
) -> Result<Merged, MergeError> {
    if !targets.iter().any(|listed| listed == target) {
        return Err(MergeError::Unlisted {
            target: target.to_string(),
            targets: targets.to_vec(),
        });
    }
    // One merge per project at a time, held until this function returns — which is the whole write.
    let lock = merges.project(&worktree.repository);
    let _held = lock.lock().unwrap_or_else(PoisonError::into_inner);

    let head = basis(worktree)?;
    let repository = worktree.repository.as_path();
    let reference = format!("refs/heads/{target}");
    let old = target_head(repository, &reference)?.ok_or_else(|| MergeError::NoTarget {
        target: target.to_string(),
    })?;
    if !is_ancestor(
        repository,
        &reference,
        &format!("refs/heads/{}", worktree.branch),
    )? {
        return Err(MergeError::NotFastForward {
            target: target.to_string(),
            branch: worktree.branch.clone(),
        });
    }
    match checkout_of(repository, target)? {
        None => update_ref(
            repository,
            &reference,
            &head,
            &old,
            target,
            &worktree.branch,
        )?,
        Some(path) => {
            if is_the_repository(&path, repository) {
                fast_forward(&path, repository, worktree, target, &head)?;
            } else {
                // Inside a session's worktree a container is working, and a host write there would move files
                // under it. The spec's rule 4 names two shapes; this is the third, and a refusal is the only
                // honest answer this build can give.
                return Err(MergeError::CheckedOutElsewhere {
                    target: target.to_string(),
                    path,
                });
            }
        }
    }
    Ok(Merged {
        merged_commit: head,
        target: target.to_string(),
        strategy: FF_ONLY.to_string(),
    })
}

/// The commit `reference` points at, or `None` when the repository has no such ref.
///
/// `rev-parse --verify --quiet` answers 1 for "no such ref" and prints nothing; anything else is a real
/// failure. The same shape as [`Worktree::branch_head`], spelled here because the branch was not this
/// module's and a `Worktree` is not a target.
fn target_head(repository: &Path, reference: &str) -> Result<Option<String>, GitError> {
    let args = ["rev-parse", "--verify", "--quiet", reference];
    let output = git_output(repository, &args)?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => Err(git_error(repository, &args, &output)),
    }
}

/// Whether `older` is an ancestor of (or equal to) `newer` — `git merge-base --is-ancestor`'s own two answers.
///
/// Exit 0 and exit 1 are the answer; anything else is a question that could not be asked, which is a failure
/// rather than a `false`. The same command HS-043's keep-rule reads, for the same reason: a build that reads
/// another program's exit code as a boolean must say which codes it is reading.
fn is_ancestor(repository: &Path, older: &str, newer: &str) -> Result<bool, GitError> {
    let args = ["merge-base", "--is-ancestor", older, newer];
    let output = git_output(repository, &args)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_error(repository, &args, &output)),
    }
}

/// The checkout that has `branch` checked out, if one has.
///
/// Three answers, because that is how many rule 4 needs: nowhere (the ref is updated), the project's own
/// checkout (the merge runs there), and anywhere else (a refusal). A detached checkout is not a checkout *of*
/// the branch, so it is not one of these — which is the property that makes the unchecked-out path reachable in
/// a repository that is always "checked out" somewhere.
fn checkout_of(repository: &Path, branch: &str) -> Result<Option<PathBuf>, GitError> {
    let found: Vec<Checkout> = checkouts(repository)?;
    Ok(found
        .into_iter()
        .find(|checkout| checkout.branch.as_deref() == Some(branch))
        .map(|checkout| checkout.path))
}

/// Whether `path` is the repository's own checkout, by git's spelling or after resolution.
///
/// `git worktree list --porcelain` prints resolved paths, and a projects root may be reached through a symlink
/// (HS-025's measurement), so the two spellings of one directory are compared after `canonicalize` when the
/// raw comparison fails. A path that cannot be resolved is not the repository: the refusal is the safe side.
fn is_the_repository(path: &Path, repository: &Path) -> bool {
    if path == repository {
        return true;
    }
    match (
        std::fs::canonicalize(path),
        std::fs::canonicalize(repository),
    ) {
        (Ok(theirs), Ok(ours)) => theirs == ours,
        _ => false,
    }
}

/// Update a target that is not checked out anywhere: the compare-and-swap of rule 4.
///
/// `old` is the commit this module read a moment ago and `new` is the branch's tip; git performs the write only
/// if the ref still holds `old`, which is what makes "the target moved" a refusal rather than a lost update.
///
/// A non-zero exit is not read as English. It is re-measured: if the ref no longer holds `old`, the target
/// moved and the answer is [`MergeError::NotFastForward`]; otherwise git failed for a reason that is nobody's
/// decision, and its own words go to the log behind a `500`.
fn update_ref(
    repository: &Path,
    reference: &str,
    new: &str,
    old: &str,
    target: &str,
    branch: &str,
) -> Result<(), MergeError> {
    let args = update_ref_arguments(reference, new, old);
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = git_output(repository, &borrowed)?;
    if output.status.success() {
        return Ok(());
    }
    match target_head(repository, reference)? {
        Some(now) if now != old => Err(MergeError::NotFastForward {
            target: target.to_string(),
            branch: branch.to_string(),
        }),
        _ => Err(MergeError::Changes(ChangesError::Git(git_error(
            repository, &borrowed, &output,
        )))),
    }
}

/// Fast-forward the target in the checkout that holds it, and refuse with the fact that stopped it.
///
/// The merge is run first and classified afterwards, which is the decision: pre-checking the checkout's status
/// would refuse a merge over an unrelated dirty file, and git's refusal text is not a contract. So when git
/// exits non-zero, this function asks two questions of its own, in this order:
///
/// 1. **is the target still an ancestor of the branch?** If not, the target moved and nothing can be
///    fast-forwarded — [`MergeError::NotFastForward`], the same answer the pre-check gives, re-measured.
/// 2. **does the checkout hold work?** [`dirty`] is the changes view's definition, asked of this directory —
///    [`MergeError::TargetDirty`]. It is asked *after* the merge failed, so an unrelated dirty file (which git
///    carries across a fast-forward happily) never reaches this arm.
///
/// Anything else is a `500` with git's own words: a fast-forward that failed for a third reason is a machine
/// problem, and guessing which of the two refusals it "really" was would be inventing a measurement.
fn fast_forward(
    checkout: &Path,
    repository: &Path,
    worktree: &Worktree,
    target: &str,
    head: &str,
) -> Result<(), MergeError> {
    let args = merge_arguments(head);
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = git_output(checkout, &borrowed)?;
    if output.status.success() {
        // The postcondition, measured rather than assumed: the response names this commit as where the target
        // now is, so the target is read once and compared. A checkout whose branch moved between the read and
        // the write is not something this build can prevent (git is the arbiter of its own index), but it is
        // something it can refuse to claim.
        let reference = format!("refs/heads/{target}");
        if target_head(repository, &reference)?.as_deref() == Some(head) {
            return Ok(());
        }
        return Err(MergeError::Changes(ChangesError::Git(GitError {
            command: git_command(checkout, &borrowed),
            detail: format!(
                "the merge ran, but {reference} is not the commit it was asked to become ({head})"
            ),
        })));
    }
    let reference = format!("refs/heads/{target}");
    if !is_ancestor(
        repository,
        &reference,
        &format!("refs/heads/{}", worktree.branch),
    )? {
        return Err(MergeError::NotFastForward {
            target: target.to_string(),
            branch: worktree.branch.clone(),
        });
    }
    if dirty(checkout)? {
        return Err(MergeError::TargetDirty {
            target: target.to_string(),
            path: checkout.to_path_buf(),
        });
    }
    Err(MergeError::Changes(ChangesError::Git(git_error(
        checkout, &borrowed, &output,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_merge_line_disables_hooks_and_is_a_fast_forward_or_nothing() {
        // Item 12's rule 2 sentence, applied to the fast-forward path: `-c core.hooksPath=/dev/null` so a
        // `post-merge` hook is never a program this build runs on an agent's behalf, and `--ff-only` so a
        // target that moved is a refusal rather than a three-way merge with somebody's working tree. The
        // vector is the whole command, options before the subcommand because `-c` is git-wide and not
        // `merge`'s.
        let expected = [
            "-c",
            "core.hooksPath=/dev/null",
            "merge",
            "--ff-only",
            "0123456789abcdef",
        ];
        assert_eq!(merge_arguments("0123456789abcdef"), expected);
    }

    #[test]
    fn one_project_has_one_lock_and_two_projects_have_two() {
        // Rule 2's "one merge per project at a time", as far as a test can see it. The *outcome* of two
        // concurrent merges cannot distinguish this lock from the compare-and-swap — both leave exactly one
        // merge winning — so what is measured is the lock's identity: one repository answers one mutex, and two
        // repositories do not. An implementation that made a fresh mutex per call, which would serialise
        // nothing, goes red here.
        let merges = Merges::new();
        let one = merges.project(Path::new("/projects/holosmith"));
        let again = merges.project(Path::new("/projects/holosmith"));
        let other = merges.project(Path::new("/projects/another"));
        assert!(Arc::ptr_eq(&one, &again));
        assert!(!Arc::ptr_eq(&one, &other));
    }

    #[test]
    fn the_update_is_a_compare_and_swap_with_the_old_value_third() {
        // Rule 4's `git update-ref refs/heads/<target> <new> <old>`: three arguments, new before old, and the
        // third is the whole race-free half. A vector that dropped `old` would still "move the ref" and would
        // still pass the unchecked-out test, so the argument list is what proves the compare-and-swap is asked
        // for at all.
        let expected = [
            "update-ref",
            "refs/heads/main",
            "0123456789abcdef",
            "fedcba9876543210",
        ];
        assert_eq!(
            update_ref_arguments("refs/heads/main", "0123456789abcdef", "fedcba9876543210"),
            expected
        );
    }
}
