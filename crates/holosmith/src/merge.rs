//! The merge: a session's branch landed on a target branch (and, spec item 12's rules 2, 3
//! and 4).
//!
//! Item 12's rules 2–4, in full, are what this module answers:
//!
//! ```text
//! Rule 2. One merge per project at a time. The server creates a temporary detached worktree at
//!         `<data>/merge/<id>` from the target, runs the merge there with `-c core.hooksPath=/dev/null` so
//!         that your hooks never run agent-written code, and removes the worktree afterwards. `ff-only`
//!         skips the temporary worktree and fast-forwards directly, failing with 409 `not_fast_forward` if
//!         the target has moved.
//! Rule 3. On conflicts the merge is aborted, nothing changes, and the call returns 409 `conflict` with the
//!         conflicting paths.
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
//!   runs and a `std` mutex across an `.await` would park a runtime worker. It **waits** rather than refusing:
//!   rule 2 allows either, and a second kind of answer for a request that is not wrong buys nothing.
//! - **The no-worktree rules are the changes view's** ([`crate::changes::basis`]): "there is no worktree" is one
//!   fact with four refusals, and the module imports that function rather than copying it. What it *adds* is the
//!   target's own facts; what it does *not* inherit is the sentences, which `http::merge` writes for its own
//!   request (a `409` on a merge that says "there is nothing to review" is about the wrong request).
//! - **Every answer is measured, never inferred from git's English.** `merge-base --is-ancestor` exits 0/1 as
//!   an answer (the same shape 's keep-rule reads), `update-ref`'s compare-and-swap is the race-free
//!   guard, a failed `git merge --ff-only` is classified by re-measuring two facts of our own — the ancestry
//!   and the checkout's status — and a non-zero exit from a **real** merge is classified by reading the index's
//!   unmerged paths ([`unmerged`]), not by looking for a sentence in git's stderr.
//! - **Hooks never run**: every command that can run one carries `-c core.hooksPath=/dev/null`, item 12's own
//!   sentence ("so that your hooks never run agent-written code"). That is the fast-forward in the operator's
//!   own checkout (`post-merge`), the temporary worktree's `worktree add` (`post-checkout`), its `merge`
//!   (`post-merge`, `prepare-commit-msg`, `commit-msg`) and its `commit` (`pre-commit`, `commit-msg`,
//!   `post-commit`). Hooks live in the **common** git directory, so a project's hook is reachable from a
//!   temporary worktree just as it is from the checkout.
//! - **One way to move a ref.** A `merge` or a `squash` builds a commit in the temporary worktree and then
//!   hands it to rule 4's two paths, unchanged: the commit descends from the target, so the target's move is
//!   still a fast-forward and this module does not learn a second write.
//!
//! What this module does not do: none of rule 1's guards and no `delete_session` — both are the *route's*
//! (ADR-0065: a session row is not this module's to read), no publish, and no UI (a later change owns
//! the Changes sheet and the Resolve-with-agent affordance — ADR-0064). It also does not touch the database: a
//! merge is neither a state transition nor an event, and the changes view is **unchanged** by one — `ahead`
//! counts the branch's commits and the branch did not move.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;

use crate::changes::{ChangesError, basis, dirty};
use crate::snapshot::IDENTITY;
use crate::worktree::{
    Checkout, GitError, Worktree, checkouts, git_command, git_error, git_output,
};

/// Item 12's default strategy, and the only one that needs no temporary worktree.
pub const FF_ONLY: &str = "ff-only";

/// The directory under the data root the temporary merge worktrees live in: `<data>/merge/<id>` (rule 2).
///
/// The module owns the name because the name is the rule — the handler knows a data root, and the shape below
/// it is this module's.
pub const MERGE_DIR: &str = "merge";

/// The `-c` option that makes a project's hooks unreachable, on every command that can run one.
///
/// One value rather than five literals: it is item 12's own sentence, and a command that forgot it would be a
/// command that runs an agent-adjacent repository's program on the host.
const HOOKS_PATH: (&str, &str) = ("core.hooksPath", "/dev/null");

/// The three strategies item 12's contract names, and the one place their words are spelled.
///
/// A type rather than three `&str` comparisons in the handler, because the strategy decides **which commands
/// run and whether a commit is made at all** — the difference between `ff-only` and `merge` is not a word in a
/// response, it is whether the target moves to the branch's tip or to a new two-parent commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// The target moves to the branch's tip, with no temporary worktree and no commit of this module's.
    FastForward,
    /// A merge commit in the temporary worktree: two parents, the old target and the branch.
    Merge,
    /// One commit in the temporary worktree whose tree is the merge's and whose only parent is the old target.
    Squash,
}

impl Strategy {
    /// The strategy `word` names, or `None` when it is not one of the contract's three.
    ///
    /// The words come from [`Strategy::as_str`], so the table exists once: a fourth word the handler admits and
    /// this does not know is impossible by construction.
    pub fn parse(word: &str) -> Option<Self> {
        [Self::FastForward, Self::Merge, Self::Squash]
            .into_iter()
            .find(|strategy| strategy.as_str() == word)
    }

    /// The strategy's word: the request's spelling and the response's `strategy`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FastForward => FF_ONLY,
            Self::Merge => "merge",
            Self::Squash => "squash",
        }
    }

    /// The landing this strategy needs — a commit made somewhere that is not the target's checkout — or
    /// `None` for the one strategy that writes no commit at all.
    fn landing(self) -> Option<Landing> {
        match self {
            Self::FastForward => None,
            Self::Merge => Some(Landing::Merge),
            Self::Squash => Some(Landing::Squash),
        }
    }
}

/// The two strategies that make a commit in the temporary worktree (rule 2), narrowed so that the code that
/// does it cannot be reached by the one that must not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Landing {
    Merge,
    Squash,
}

/// One merge request: item 12's three fields, with the strategy and the message already interpreted.
///
/// The body's shape is `http::merge`'s to refuse — an unknown key, a non-string, a fourth strategy word — and
/// what survives that is this. Owned rather than borrowed because it is built from a request body and then
/// moved into the blocking task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The **Target branch** the session's work lands on. Always one of `[merge] targets`.
    pub target: String,
    /// Which of the three strategies runs; absent in the body means [`Strategy::FastForward`].
    pub strategy: Strategy,
    /// The commit message for a strategy that makes a commit, passed through **verbatim**.
    ///
    /// `None` is not "no message": it is "the default", which is [`message_for`] — one sentence shape for both
    /// landings, because git's own default for a detached worktree merging a fully qualified ref is `Merge branch
    /// 'refs/heads/agent/<id>' into HEAD` and a squash has no default at all. A fast-forward ignores it, because
    /// it makes no commit for a message to name.
    pub message: Option<String>,
}

/// The one `git merge` line the **fast-forward** runs, in the checkout that holds the target.
///
/// `-c core.hooksPath=/dev/null` because a `post-merge` hook in a project repository is a program the host
/// runs, and after an agent has been writing into `/work` the operator's hooks are not code this build is
/// willing to execute on its behalf. `--ff-only` is the strategy's whole meaning: there is no merge commit, so
/// a target that has moved past the branch is a refusal rather than a three-way merge.
///
/// The argument order is the decision: options before the subcommand (`-c` is a git-wide option, not `merge`'s),
/// and the commit last.
fn fast_forward_arguments(new: &str) -> Vec<String> {
    let mut args = hooks_flag();
    args.extend([
        "merge".to_string(),
        "--ff-only".to_string(),
        new.to_string(),
    ]);
    args
}

/// `-c core.hooksPath=/dev/null`, as the two arguments git wants.
fn hooks_flag() -> Vec<String> {
    [
        "-c".to_string(),
        format!("{}={}", HOOKS_PATH.0, HOOKS_PATH.1),
    ]
    .to_vec()
}

/// The `-c` options every command the **landing** runs carries: hooks never run, and a commit this build
/// makes carries holosmith's identity rather than the machine's git config.
///
/// The identity is [`IDENTITY`] — the WIP snapshot's own pair — for the reason it exists there: a merge commit
/// is written by this build on an operator's behalf, and a machine whose git has no `user.name` (a fresh
/// container, a bare server) would otherwise fail to write one at all. `-c` on the command, never a config
/// write.
fn landing_prefix() -> Vec<String> {
    let mut args = hooks_flag();
    for (key, value) in IDENTITY {
        args.push("-c".to_string());
        args.push(format!("{key}={value}"));
    }
    args
}

/// `git worktree add --detach <path> refs/heads/<target>`: the temporary worktree, at **the target's commit**.
///
/// `--detach` because the target is already checked out somewhere (or may be, at any moment) and git refuses to
/// check one branch out twice; at the target's commit because that is what "merge the branch **into** the
/// target" starts from. `refs/heads/<target>` fully qualified so a tag named `main` cannot be what is merged.
fn worktree_add_arguments(path: &Path, target: &str) -> Vec<String> {
    let mut args = landing_prefix();
    args.extend([
        "worktree".to_string(),
        "add".to_string(),
        "--detach".to_string(),
        path.display().to_string(),
        format!("refs/heads/{target}"),
    ]);
    args
}

/// `git merge --no-ff -m <message> refs/heads/<branch>`: a merge commit, always.
///
/// `--no-ff` is what makes this a strategy rather than a second spelling of `ff-only`: a branch that could be
/// fast-forwarded still gets a commit with two parents, which is the answer the contract's `merge` promises.
///
/// The message is **always** given, and that is a measurement rather than a taste: left to itself git writes
/// `Merge branch 'refs/heads/agent/<id>' into HEAD`, because the temporary worktree is detached and the
/// argument is a fully qualified ref. That sentence is about this module's scratch directory, not about the
/// operator's branch, so the build writes [`merge_message`] instead — the same shape as a squash's.
fn merge_arguments(branch: &str, message: &str) -> Vec<String> {
    let mut args = landing_prefix();
    args.extend([
        "merge".to_string(),
        "--no-ff".to_string(),
        "-m".to_string(),
        message.to_string(),
        format!("refs/heads/{branch}"),
    ]);
    args
}

/// `git merge --squash refs/heads/<branch>`: the merge's result in the index and the working tree, no commit.
fn squash_arguments(branch: &str) -> Vec<String> {
    let mut args = landing_prefix();
    args.extend([
        "merge".to_string(),
        "--squash".to_string(),
        format!("refs/heads/{branch}"),
    ]);
    args
}

/// `git commit -m <message>`: the squash's one commit, whose only parent is the temporary worktree's HEAD.
fn commit_arguments(message: &str) -> Vec<String> {
    let mut args = landing_prefix();
    args.extend(["commit".to_string(), "-m".to_string(), message.to_string()]);
    args
}

/// The message a landing carries when the request names none.
///
/// One shape for both strategies, naming both refs, because git's own default is not available to this build:
/// the temporary worktree is detached and the merge argument is fully qualified, so git would write
/// `Merge branch 'refs/heads/agent/<id>' into HEAD` — a sentence about the scratch directory. `Squash … into …`
/// would have no default at all (`--squash` stops before committing), so the build writes the one sentence the
/// two strategies share rather than two different inventions.
fn message_for(landing: Landing, worktree: &Worktree, target: &str) -> String {
    match landing {
        Landing::Merge => format!("Merge {} into {target}", worktree.branch),
        Landing::Squash => format!("Squash {} into {target}", worktree.branch),
    }
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
/// `merged_commit` is the target's commit **after** the merge — the answer to "where did the work land". For
/// `ff-only` it is the branch's tip, and for a branch that was already merged it is the commit the target was
/// at, because a fast-forward of an ancestor is a no-op rather than an error. For `merge` and `squash` it is
/// the commit this build wrote in the temporary worktree, and it is the same commit as the target's when the
/// merge had nothing to do. `strategy` is the strategy that ran, which is the one that was asked for (or the
/// default that was not).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Merged {
    /// The commit the target now points at.
    pub merged_commit: String,
    /// The branch that moved.
    pub target: String,
    /// The strategy that ran: one of [`Strategy::as_str`]'s three words.
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
    /// The merge conflicted, in these paths (rule 3). The only refusal whose payload is a **list**: the
    /// envelope's message is a fixed sentence (ADR-0008), so the paths travel in an additive `conflicts` key
    /// and in this log line.
    Conflict { paths: Vec<String> },
    /// The temporary worktree could not be made or removed — a filesystem fact rather than an operator's
    /// decision, and therefore a `500` rather than a `409`.
    Temporary { path: PathBuf, detail: String },
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
            Self::Conflict { paths } => {
                if paths.is_empty() {
                    "the merge conflicts, and git named no path it could not merge".to_string()
                } else {
                    format!("the merge conflicts in: {}", paths.join(", "))
                }
            }
            Self::Temporary { path, detail } => format!(
                "the temporary merge worktree {} could not be used: {detail}",
                path.display()
            ),
        }
    }
}

/// Land a session's branch on one of the configured targets, by the requested strategy, and answer where the
/// work landed.
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
/// 5. **the strategy**: `ff-only` first asks whether the target is an ancestor of the branch — a fact about the
///    request, and a refusal that costs nothing — and writes no commit; the other two build a commit in the
///    temporary worktree ([`land`]), which is why *their* checkout question comes first;
/// 6. **where the target is checked out** — the repository's own checkout, nowhere, or somewhere else. Asked
///    before the landing, so a `409 target_checked_out` costs no temporary worktree at all;
/// 7. **the write**, by one of rule 4's two commands — the same two for all three strategies, because a merge
///    commit descends from the target and is therefore itself a fast-forward of it.
///
/// A session row is *not* consulted and *not* written: this function knows a worktree, a request and the
/// config's list. Rule 1's two guards — a running turn and the session's own uncommitted work — are the
/// **route's**, because they are facts about a row and this module is handed none (ADR-0065 D1); the
/// route asks them before calling this, which is also why a refused merge makes no temporary directory at all.
pub fn merge(
    worktree: &Worktree,
    request: &Request,
    targets: &[String],
    merges: &Merges,
    data_root: &Path,
) -> Result<Merged, MergeError> {
    let target = request.target.as_str();
    if !listed(target, targets) {
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
    let landing = request.strategy.landing();
    // The fast-forward test comes **before** the checkout question, and that order is that story's: "the target has
    // commits the branch does not" is a fact about the request, while `target_checked_out`'s advice ("check out
    // another branch in that worktree") would send the operator to a second refusal. A `merge` or a `squash` does
    // not ask it — a target that has moved on is the ordinary case a real merge exists for — and for those the
    // checkout question comes first, so a `409 target_checked_out` costs no temporary worktree at all.
    if landing.is_none() && !is_ancestor(repository, &reference, &head)? {
        return Err(MergeError::NotFastForward {
            target: target.to_string(),
            branch: worktree.branch.clone(),
        });
    }
    // Where the write will land. Inside a session's worktree a container is working, and a host write there would
    // move files under it. The spec's rule 4 names two shapes; this is the third, and a refusal is the only honest
    // answer this build can give.
    let site = match checkout_of(repository, target)? {
        None => Site::Ref,
        Some(path) if is_the_repository(&path, repository) => Site::Checkout(path),
        Some(path) => {
            return Err(MergeError::CheckedOutElsewhere {
                target: target.to_string(),
                path,
            });
        }
    };
    let new = match landing {
        // The commit this merge will write is the branch's tip, and the ancestry test above was asked about it.
        None => head.clone(),
        Some(landing) => land(repository, worktree, request, landing, data_root)?,
    };
    match site {
        Site::Ref => update_ref(repository, &reference, &new, &old, target, &worktree.branch)?,
        Site::Checkout(path) => fast_forward(&path, repository, worktree, target, &new)?,
    }
    Ok(Merged {
        merged_commit: new,
        target: target.to_string(),
        strategy: request.strategy.as_str().to_string(),
    })
}

/// Where the target's move is written: the ref (no checkout holds it) or a checkout (rule 4's second arm).
///
/// A value rather than an `Option<PathBuf>` because the two arms are two different commands and the *third*
/// answer — a checkout that is not the project's own — is a refusal that happens above, before anything is
/// built.
enum Site {
    Ref,
    Checkout(PathBuf),
}

/// Build the commit a `merge` or a `squash` lands, in the temporary worktree, and answer its id.
///
/// Rule 2's sequence, in order, and every step is measured rather than trusted:
///
/// 1. `<data>/merge/<id>` made (the parent `<data>/merge` created when it is not there);
/// 2. a **stale** directory from a killed run removed — never refused, because a leftover directory is not a
///    question about concurrency and `409 merge_in_progress` would answer the wrong one ([`clear`]);
/// 3. `git worktree add --detach <path> refs/heads/<target>`, so the worktree starts at the target's commit;
/// 4. the strategy's commands ([`apply`]);
/// 5. `HEAD`, read from the worktree — the commit this landing produced, which is the target's own commit when
///    there was nothing to do;
/// 6. **the worktree removed**, and it is removed *here*, before the target moves: the acceptance is "removed
///    afterwards even on failure", and the move below is a thing that can fail (`409 target_dirty`) long after
///    the landing succeeded.
fn land(
    repository: &Path,
    worktree: &Worktree,
    request: &Request,
    landing: Landing,
    data_root: &Path,
) -> Result<String, MergeError> {
    let directory = data_root.join(MERGE_DIR);
    std::fs::create_dir_all(&directory).map_err(|error| MergeError::Temporary {
        path: directory.clone(),
        detail: error.to_string(),
    })?;
    let path = directory.join(&worktree.id);
    clear(repository, &path)?;
    add(repository, &path, &request.target)?;
    let landed = apply(&path, worktree, request, landing);
    remove(repository, &path);
    landed
}

/// Run the strategy's commands in the temporary worktree, and answer the commit they produced.
///
/// A `merge` is one command and always makes a commit (`--no-ff`). A `squash` is two — the merge into the index
/// and then the commit — and its second can legitimately fail: `git commit` refuses to write a commit that says
/// nothing, which is what an already-squashed branch produces. That one case is **measured**
/// ([`nothing_staged`]) and answered as a no-op naming the target's own commit; anything else is git's failure.
fn apply(
    path: &Path,
    worktree: &Worktree,
    request: &Request,
    landing: Landing,
) -> Result<String, MergeError> {
    let old = head_of(path)?;
    let message = request
        .message
        .clone()
        .unwrap_or_else(|| message_for(landing, worktree, &request.target));
    match landing {
        Landing::Merge => {
            run_in(path, &merge_arguments(&worktree.branch, &message))?;
        }
        Landing::Squash => {
            run_in(path, &squash_arguments(&worktree.branch))?;
            match run_in(path, &commit_arguments(&message)) {
                Ok(()) => {}
                Err(error) => {
                    // The one benign refusal: the merge staged no change, so there is nothing to commit. It is
                    // measured, not read — git's sentence for it is prose, and prose is not a contract.
                    if !nothing_staged(path)? {
                        return Err(error);
                    }
                }
            }
        }
    }
    let new = head_of(path)?;
    // A landing that produced no commit is a no-op, and the answer says so by naming the target's own commit
    // rather than by inventing a refusal for a request that is already satisfied.
    Ok(if new == old { old } else { new })
}

/// Whether `target` is one of the branches the config lets this machine merge into (item 12's rule 6).
///
/// One predicate with two callers, and the pair is deliberate: the handler asks it **before** it reads the
/// session, because a request naming a branch this machine will not merge into is malformed whatever the
/// session's state is, and [`merge`] asks it again as its own precondition — a module that would merge into any
/// name it was handed is a module whose refusal depends on its caller. Both answers are the same `422`; the
/// handler's is the one a client sees first.
pub fn listed(target: &str, targets: &[String]) -> bool {
    targets.iter().any(|listed| listed == target)
}

/// Run one git command in the **temporary worktree** and say why it failed.
///
/// Exit zero is success. Anything else is classified by measuring the index, never by reading git's stderr: a
/// merge that could not be completed leaves unmerged paths there, so a non-zero exit **with** such paths is
/// [`MergeError::Conflict`] and the same exit **without** them is a failure nobody decided — a `500` carrying
/// git's own words. The two facts are different answers to different questions, which is why they are
/// measured apart.
fn run_in(directory: &Path, args: &[String]) -> Result<(), MergeError> {
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = git_output(directory, &borrowed)?;
    if output.status.success() {
        return Ok(());
    }
    let paths = unmerged(directory)?;
    if !paths.is_empty() {
        return Err(MergeError::Conflict { paths });
    }
    Err(MergeError::Changes(ChangesError::Git(git_error(
        directory, &borrowed, &output,
    ))))
}

/// The paths a merge could not reconcile, read from the temporary worktree's **index**:
/// `git ls-files --unmerged -z`.
///
/// `ls-files` and not `git diff --diff-filter=U`, because a diff is where a repository's own config gets a
/// say: `diff.external` replaces the whole output with a program's, and a textconv driver rewrites a file's
/// bytes (ADR-0062 measured both on this build). A conflict is a fact about the index, so it is asked of the
/// command that reads the index and of nothing else.
fn unmerged(directory: &Path) -> Result<Vec<String>, MergeError> {
    let args = ["ls-files", "--unmerged", "-z"];
    let output = git_output(directory, &args)?;
    if !output.status.success() {
        return Err(MergeError::Changes(ChangesError::Git(git_error(
            directory, &args, &output,
        ))));
    }
    Ok(conflict_paths(&output.stdout))
}

/// The paths in `ls-files --unmerged -z`'s output: `<mode> <sha> <stage>\t<path>\0`, once per stage.
///
/// The record is split at the tab — the same shape `changes::conflict_paths` reads from `merge-tree` — and the
/// paths are deduped, because a path with a conflict at two stages appears twice and the operator acts on it
/// once. A record without a tab is not a path, so it is skipped rather than reported.
fn conflict_paths(stdout: &[u8]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for record in stdout.split(|byte| *byte == 0) {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let path = String::from_utf8_lossy(&record[tab + 1..]).to_string();
        if path.is_empty() || paths.contains(&path) {
            continue;
        }
        paths.push(path);
    }
    paths
}

/// Whether the temporary worktree's index holds **no** change against its `HEAD`.
///
/// `git diff-index --cached --quiet HEAD`'s two exit codes: 0 means the two agree — so a `squash` whose merge
/// staged nothing has nothing to commit and is a no-op — and 1 means there is work to commit. Anything else is
/// a question that could not be asked, which is a failure rather than a `true`. The plumbing command, because
/// it is a question about the index and a porcelain `diff` would let a repository's config answer it.
fn nothing_staged(directory: &Path) -> Result<bool, MergeError> {
    let args = ["diff-index", "--cached", "--quiet", "HEAD"];
    let output = git_output(directory, &args)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(MergeError::Changes(ChangesError::Git(git_error(
            directory, &args, &output,
        )))),
    }
}

/// The commit the temporary worktree is at: `git rev-parse HEAD`.
fn head_of(directory: &Path) -> Result<String, MergeError> {
    let args = ["rev-parse", "HEAD"];
    let output = git_output(directory, &args)?;
    if !output.status.success() {
        return Err(MergeError::Changes(ChangesError::Git(git_error(
            directory, &args, &output,
        ))));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Remove whatever is at the temporary worktree's path — a killed run's leftovers included — and prune the
/// registrations git can no longer place.
///
/// Two steps, and the **measurement** between them is `path.exists()` rather than git's exit status: git may
/// not know the path at all (an operator's `rm -rf`, a half-written first attempt), in which case
/// `worktree remove` fails for the right reason and the directory is still ours to delete. Then `worktree
/// prune`, unconditionally, because the *other* shape is a registration whose directory is gone — git calls it
/// prunable and `worktree add` refuses a path git has such a registration for. Prune is a no-op for every
/// registration whose directory is still there, which is every live worktree on this machine.
fn clear(repository: &Path, path: &Path) -> Result<(), MergeError> {
    if path.exists() {
        let _ = git_output(
            repository,
            &["worktree", "remove", "--force", &path.display().to_string()],
        );
        if path.exists() {
            std::fs::remove_dir_all(path).map_err(|error| MergeError::Temporary {
                path: path.to_path_buf(),
                detail: format!("a stale directory could not be removed ({error})"),
            })?;
        }
    }
    let args = ["worktree", "prune"];
    let output = git_output(repository, &args)?;
    if !output.status.success() {
        return Err(MergeError::Changes(ChangesError::Git(git_error(
            repository, &args, &output,
        ))));
    }
    Ok(())
}

/// Create the temporary worktree, detached at the target's commit.
fn add(repository: &Path, path: &Path, target: &str) -> Result<(), MergeError> {
    let args = worktree_add_arguments(path, target);
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = git_output(repository, &borrowed)?;
    if output.status.success() {
        return Ok(());
    }
    Err(MergeError::Changes(ChangesError::Git(git_error(
        repository, &borrowed, &output,
    ))))
}

/// Remove the temporary worktree, and **never fail the request** for it.
///
/// The answer is already decided when this runs, and a merge that landed is a merge that landed: a directory
/// this could not delete is a `warn!` with its path (the next merge of this session clears it — [`clear`]), not
/// a `500` that would tell the operator their work did not land. A conflicted worktree is dirty, which is what
/// `--force` is for; a directory git does not know is deleted directly.
fn remove(repository: &Path, path: &Path) {
    let spelled = path.display().to_string();
    let _ = git_output(repository, &["worktree", "remove", "--force", &spelled]);
    if path.exists() {
        let _ = std::fs::remove_dir_all(path);
        let _ = git_output(repository, &["worktree", "prune"]);
    }
    if path.exists() {
        tracing::warn!(
            target: "holosmith::merge",
            path = %path.display(),
            "the temporary merge worktree could not be removed; the next merge of this session will clear it"
        );
    }
}
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
/// rather than a `false`. The same command the keep-rule reads, for the same reason: a build that reads
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
        // A **prunable** registration has no working tree — the directory is gone and `git worktree prune` has
        // not run — so the target is not really checked out there and there is nothing to move. Counting it
        // would refuse a merge the operator can perform, with advice ("check out another branch in that
        // worktree") that cannot be followed; the ref is the right arm instead. the landmine is exactly
        // such a registration.
        .filter(|checkout| !checkout.prunable)
        .find(|checkout| checkout.branch.as_deref() == Some(branch))
        .map(|checkout| checkout.path))
}

/// Whether `path` is the repository's own checkout, by git's spelling or after resolution.
///
/// `git worktree list --porcelain` prints resolved paths, and a projects root may be reached through a symlink
/// so the two spellings of one directory are compared after `canonicalize` when the
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
    let args = fast_forward_arguments(head);
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
    // Re-measured about the same commit the merge was asked to write, so the question is "is the target still
    // an ancestor of `head`" rather than about wherever the branch has got to since.
    let reference = format!("refs/heads/{target}");
    if !is_ancestor(repository, &reference, head)? {
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

    fn texts(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    #[test]
    fn the_fast_forward_line_disables_hooks_and_is_a_fast_forward_or_nothing() {
        // Item 12's rule 2 sentence, applied to the fast-forward path: `-c core.hooksPath=/dev/null` so a
        // `post-merge` hook is never a program this build runs on an agent's behalf, and `--ff-only` so a
        // target that moved is a refusal rather than a three-way merge with somebody's working tree. The
        // vector is the whole command, options before the subcommand because `-c` is git-wide and not
        // `merge`'s. No identity, because a fast-forward writes no commit for one to belong to.
        assert_eq!(
            fast_forward_arguments("0123456789abcdef"),
            [
                "-c",
                "core.hooksPath=/dev/null",
                "merge",
                "--ff-only",
                "0123456789abcdef",
            ]
        );
    }

    #[test]
    fn the_landing_prefix_disables_hooks_and_carries_this_build_s_identity() {
        // Every command the temporary worktree runs can run a hook — `worktree add` runs `post-checkout`, a
        // `merge` runs four of them, a `commit` four more — and hooks live in the **common** git directory, so a
        // project's hook is reachable from the temporary worktree. The identity is `snapshot::IDENTITY`: a
        // commit this build writes is holosmith's, and a machine whose global git config has no `user.name`
        // (a fresh container, a bare server) would otherwise fail to write one at all.
        assert_eq!(
            texts(&landing_prefix()),
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=holosmith",
                "-c",
                "user.email=holosmith@localhost",
            ]
        );
    }

    #[test]
    fn the_worktree_is_detached_at_the_target_ref() {
        // `--detach` because the target may be checked out anywhere at any moment and git refuses to check one
        // branch out twice; at `refs/heads/<target>` and not at a bare name, so a tag called `main` cannot be
        // what a merge lands into.
        assert_eq!(
            worktree_add_arguments(Path::new("/data/merge/0123456789abcdef"), "main"),
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=holosmith",
                "-c",
                "user.email=holosmith@localhost",
                "worktree",
                "add",
                "--detach",
                "/data/merge/0123456789abcdef",
                "refs/heads/main",
            ]
        );
    }

    #[test]
    fn a_merge_is_no_ff_and_always_carries_a_message() {
        // `--no-ff` makes `merge` a strategy rather than a second spelling of the default. The message is always
        // passed, and that is the measurement this test records: with no `-m` git writes `Merge branch
        // 'refs/heads/agent/<id>' into HEAD`, because the temporary worktree is detached — a sentence about the
        // scratch directory. `message_for` writes the operator's instead.
        assert_eq!(
            merge_arguments(
                "agent/0123456789abcdef",
                "Merge agent/0123456789abcdef into main"
            ),
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=holosmith",
                "-c",
                "user.email=holosmith@localhost",
                "merge",
                "--no-ff",
                "-m",
                "Merge agent/0123456789abcdef into main",
                "refs/heads/agent/0123456789abcdef",
            ]
        );
    }

    #[test]
    fn a_landing_s_default_message_names_both_refs_and_the_strategy() {
        // The `-m` argument is the operator's when the request gives one, and this sentence when it does not — for
        // both strategies, in one shape, because git's own default is about this module's scratch directory.
        let worktree = Worktree {
            id: "0123456789abcdef".to_string(),
            repository: PathBuf::from("/projects/holosmith"),
            path: PathBuf::from("/projects/.worktrees/0123456789abcdef"),
            branch: "agent/0123456789abcdef".to_string(),
        };
        assert_eq!(
            message_for(Landing::Merge, &worktree, "main"),
            "Merge agent/0123456789abcdef into main"
        );
        assert_eq!(
            message_for(Landing::Squash, &worktree, "main"),
            "Squash agent/0123456789abcdef into main"
        );
    }

    #[test]
    fn a_squash_is_a_squash_merge_then_one_commit() {
        // Rule 2's two commands for this strategy: the merge into the index, and the commit whose only parent
        // is the temporary worktree's HEAD — so its tree is the branch's and its history is one commit long.
        assert_eq!(
            &squash_arguments("agent/0123456789abcdef")[6..],
            [
                "merge".to_string(),
                "--squash".to_string(),
                "refs/heads/agent/0123456789abcdef".to_string()
            ]
        );
        assert_eq!(
            &commit_arguments("Squash agent/x into main")[6..],
            [
                "commit".to_string(),
                "-m".to_string(),
                "Squash agent/x into main".to_string()
            ]
        );
    }

    #[test]
    fn the_three_strategy_words_are_one_table() {
        // `parse` and `as_str` are one table read two ways, so a word the handler admits cannot be a word this
        // module does not know. `ff-only` is the default's spelling and the response's word for the strategy
        // that ran.
        for strategy in [Strategy::FastForward, Strategy::Merge, Strategy::Squash] {
            assert_eq!(Strategy::parse(strategy.as_str()), Some(strategy));
        }
        assert_eq!(Strategy::FastForward.as_str(), FF_ONLY);
        assert_eq!(Strategy::parse("rebase"), None);
        assert_eq!(Strategy::parse(FF_ONLY.to_uppercase().as_str()), None);
    }

    #[test]
    fn the_conflict_paths_are_read_once_per_path_and_not_per_stage() {
        // `ls-files --unmerged -z` prints one record per stage, so a path conflicted at stages 1, 2 and 3 would
        // otherwise be named three times; `git` prints the path after a tab, and a record without one is not a
        // path at all.
        let output = b"100644 aaaa 1\tf.txt\x00100644 bbbb 2\tf.txt\x00100644 cccc 3\tf.txt\x00\
100644 dddd 1\tsub/g.txt\x00";
        assert_eq!(conflict_paths(output), vec!["f.txt", "sub/g.txt"]);
        assert_eq!(conflict_paths(b""), Vec::<String>::new());
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
