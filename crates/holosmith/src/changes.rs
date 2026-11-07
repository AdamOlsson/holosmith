//! The changes view's summary: what a session changed, and whether it can merge (HS-052, spec item 12).
//!
//! Item 12's first bullet, in full:
//!
//! ```text
//! GET /api/sessions/:id/changes -> {branch, base_commit, head, target, ahead, behind, dirty,
//!                                   mergeable, files:[{path, status, additions, deletions}],
//!                                   touches_ci}
//! ```
//!
//! **Every read here is host git in the project repository**, which is item 12's own sentence — "all git work
//! for review and merge runs on the host in the project repository; containers never merge" — and the reason
//! nothing in this module knows a backend. The worktree is the host's directory (ADR-0029), the branch is a
//! ref in the host's repository, and a container is asked nothing at all.
//!
//! The module is a *reader*: it takes a [`Worktree`], the row's `base_commit` and the configured
//! `[merge] targets`, and answers a [`Changes`] or a [`ChangesError`]. The handler that turns one into an
//! HTTP answer is `http::changes`, and the two are separate for the reason `http/tool_output.rs` is separate
//! from `tool.rs`: a repository in a `TempDir` can prove every rule here with no daemon, no backend and no
//! request.
//!
//! Five readings, and each one has a decision behind it (ADR-0061):
//!
//! - **`ahead`** is [`Worktree::ahead`] — `rev-list --count <base>..agent/<id>` — reused rather than
//!   re-spelled, because "ahead" has one definition in this build (HS-025's command, the glossary's entry).
//! - **`behind`** is the same question from the other side: `rev-list --count agent/<id>..<target>`, the
//!   commits the target has that the branch does not.
//! - **`files[]`** is `diff --numstat` + `diff --name-status` between the base commit and the head, i.e. the
//!   two trees the answer names (two-dot), **not** "since the merge base" (three-dot). The recorded base is
//!   the commit the branch was created from, so the two spellings agree; if an agent rewrote history inside
//!   `/work` they would not, and then "these two trees" is still the honest answer to "what did it change".
//! - **`dirty`** is `status --porcelain` *in the worktree*, and it counts an untracked file and not an
//!   ignored one — exactly the definition of "work" the WIP snapshot's `add -A` (ADR-0049) and the delete's
//!   `unsaved_work` already use, so `dirty: true` means "the merge's rule 1 would refuse".
//! - **`mergeable`** is `merge-tree --write-tree`, item 12's own command, which writes a tree object and
//!   touches no working tree. It is a **fallible** question — no target, a conflict, a git too old, a
//!   repository git cannot read — so the field is a `bool | null` with a sibling sentence, never a `false`
//!   for "I could not look" (HS-050's review, T1: a `bool` over a fallible question invents a measurement).
//!
//! Three readings are made host-safe on purpose, and each one is a bug if it is removed:
//!
//! - **`--no-ext-diff --no-textconv --no-renames`** on both diff reads, so a repository's `diff.external`, a
//!   `.gitattributes` textconv filter or a rename detection cannot change the numbers a client sees.
//! - **`-z`** so `core.quotePath` cannot hand back a quoted path — the same class as HS-025's
//!   `worktree list --porcelain` measurement, where a path with a non-ASCII byte is printed raw.
//! - **`--untracked-files=normal`** on the status and **`--no-optional-locks`** on the whole command, so a
//!   repository config cannot hide an untracked file and a read taken while the agent is committing does not
//!   fail because git wanted to refresh an index somebody else holds.
//!
//! The module's **second** answer is the unified diff itself ([`diff`], HS-053): `GET
//! /api/sessions/:id/changes/diff?path=` — item 12's next bullet, "the unified diff, capped at 256 KB, computed
//! with `--no-ext-diff --no-textconv`". It shares this module for the reason [`basis`] exists rather than being
//! copied: the refusals and the base/head resolution are one rule, and a second file with its own copy would be
//! a second answer to "what is this session's diff base". What it adds is a *bounded* read ([`DIFF_CAP`] kept,
//! [`COUNT_MAX`] counted), a marker that says how much was withheld, and one new refusal — a `path` that is not
//! in [`Changes::files`].
//!
//! What this module does not do: no write, no fetch, no lock. The merge is HS-054's, and `touches_ci`'s
//! *consumer* is HS-058's (which reads [`CI_PATHS`] through [`touches_ci`], so the warning before a publish
//! cannot become a second predicate).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::worktree::{GitError, Worktree, git_command, git_error, git_error_from, git_output};

/// The paths a publish warns about (spec item 12's `touches_ci`), in one place.
///
/// An entry is either a **directory prefix** (it ends with `/`) or a **file name** (matched against the last
/// segment of a changed path), which is why `Jenkinsfile` counts wherever a repository keeps it and
/// `.github/` counts for every workflow under it.
///
/// **`.git/hooks` is deliberately not in the set.** A diff can never contain a path under `.git/` — git does
/// not track its own directory — so "hook directories" means the ones a repository keeps in its *own* tree,
/// which is what `.husky/` and `.githooks/` are. Publish (HS-058) reads this constant through
/// [`touches_ci`]; a second predicate would be a second answer to one question.
pub const CI_PATHS: &[&str] = &[
    ".github/",
    ".gitlab/",
    ".circleci/",
    ".buildkite/",
    ".woodpecker/",
    ".husky/",
    ".githooks/",
    ".gitlab-ci.yml",
    ".travis.yml",
    ".drone.yml",
    ".cirrus.yml",
    "azure-pipelines.yml",
    "bitbucket-pipelines.yml",
    "appveyor.yml",
    "codecov.yml",
    "Jenkinsfile",
];

/// The two diff reads' flags: what the command measures, not what a repository config says it should.
///
/// `--no-ext-diff` because `diff.external` would replace git's own numstat with somebody's program,
/// `--no-textconv` because a `.gitattributes` filter would make a binary file look textual, and
/// `--no-renames` because a rename prints `{old => new}` in the path column of both reads — one spelling per
/// path is what lets the two reads be joined, and a delete plus an add is a truthful account of the lines.
const DIFF_FLAGS: [&str; 3] = ["--no-ext-diff", "--no-textconv", "--no-renames"];

/// The most bytes of a patch one response carries: spec item 12's 256 KB.
///
/// A constant rather than a config key, for the reason [`crate::tool::CAP`] is: it is a rule of the contract,
/// not a property of a machine. It is the cap on the **whole body**, the marker included — a response that is
/// 256 KB plus a marker line would not be "capped at 256 KB" — which is why [`assemble`] budgets the marker's
/// own length out of the cut.
pub const DIFF_CAP: usize = 256 * 1024;

/// The most bytes of git's output this module will *count* before it kills the child.
///
/// The cap protects the phone; this protects the server. A patch is read in chunks, never buffered whole, so
/// memory is [`DIFF_CAP`] however large the diff is — but the marker names the total size, and a total can only
/// be counted by reading to the end. Eight megabytes is far past any patch a person reads on a phone (a whole
/// source tree's diff is smaller than that), so every real answer gets an exact number, and a diff an agent
/// made out of a multi-gigabyte file costs milliseconds of counting rather than a request that runs for however
/// long the file is.
pub const COUNT_MAX: usize = 8 * DIFF_CAP;

/// How much of the child's stdout is read at a time. Large enough that the loop is not the cost, small enough
/// that it is not a copy of the patch.
const CHUNK: usize = 64 * 1024;

/// `--no-color` is the one flag the patch needs that the two counting reads do not: a repository's
/// `color.ui = always` would otherwise put escape codes in a body meant to be read on a phone. It is appended
/// in [`patch_arguments`] rather than added to [`DIFF_FLAGS`], because the counts are never coloured and this
/// way the three flags that change an *answer* stay one list.
const NO_COLOR: &str = "--no-color";

/// One file a session changed, as the changes sheet lists it.
///
/// `status` is git's own one-letter word (`A`, `M`, `D`, `T`) — a status this module invented would be a
/// second vocabulary for a fact git already names. `additions`/`deletions` are `None` exactly when git could
/// not count lines: a binary file, which `--numstat` prints as `-`/`-`. `0` there would be a lie ("no lines
/// changed") about a file that changed entirely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileChange {
    pub path: String,
    pub status: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

/// The changes view's answer: item 12's eleven keys, plus the one sentence a fallible `mergeable` needs.
///
/// The field order is the spec's, and serialising this value is the endpoint's body. Three fields are
/// nullable and the nulls are answers rather than absences:
///
/// - `target: null` — this repository has none of the configured `[merge] targets`.
/// - `behind: null` — there is no target to be behind, so the count has no meaning. (`0` would say "the
///   target has nothing the branch lacks", which is a claim about a branch that is not there.)
/// - `mergeable: null` with `mergeable_reason` — the merge question could not be *asked* (see
///   [`Merge::Unasked`]). `false` is reserved for "asked, and the answer is a conflict".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Changes {
    /// `agent/<id>`: the session branch, the glossary's **Session branch**.
    pub branch: String,
    /// The row's `base_commit`: the commit the branch started from, and the diff's basis.
    pub base_commit: String,
    /// The branch's tip — **not** the worktree's checkout, which differs exactly while a turn is mid-flight.
    pub head: String,
    /// The first existing branch of `[merge] targets`, or `None` when this repository has none of them.
    pub target: Option<String>,
    /// [`Worktree::ahead`]: commits on the branch the base does not have.
    pub ahead: u64,
    /// Commits on the target the branch does not have, or `None` with no target.
    pub behind: Option<u64>,
    /// Any uncommitted work in the worktree — untracked counts, ignored does not.
    pub dirty: bool,
    /// `merge-tree --write-tree`'s verdict: `Some(true)` clean, `Some(false)` a conflict, `None` unasked.
    pub mergeable: Option<bool>,
    /// Why `mergeable` is `false` or `None`; `None` when the merge is clean.
    pub mergeable_reason: Option<String>,
    /// Every path the branch changed against its base, in git's path order.
    pub files: Vec<FileChange>,
    /// Whether the diff touches CI configuration or a hook directory ([`CI_PATHS`]).
    pub touches_ci: bool,
}

impl Changes {
    /// Answer the changes view for one session's worktree.
    ///
    /// The order is the refusals' — cheapest and most fundamental first — and each one is asked *before*
    /// anything that would interpret its answer:
    ///
    /// 1. the repository is there (a project checkout that has gone away);
    /// 2. the worktree directory is there (a `rm -rf`, or a teardown's leftovers);
    /// 3. git registers it as a worktree of the repository (a `git worktree prune`, after which every read in
    ///    that directory would fail with "not a git repository");
    /// 4. the branch is there (a name the repository no longer holds);
    /// 5. and only then the five readings.
    ///
    /// A failure to *ask* git — git not installed, a repository git cannot read — is [`ChangesError::Git`],
    /// which the handler answers as a `500`; everything above is a state of the session or the machine, which
    /// is a `409`. That split is the project refusals' own rule (a machine problem is not a refusal).
    pub fn read(
        worktree: &Worktree,
        base_commit: &str,
        targets: &[String],
    ) -> Result<Self, ChangesError> {
        let head = basis(worktree)?;
        let repository = worktree.repository.as_path();
        let ahead = worktree.ahead(base_commit).map_err(ChangesError::Git)?;
        let files = files(repository, base_commit, &head).map_err(ChangesError::Git)?;
        let dirty = dirty(&worktree.path).map_err(ChangesError::Git)?;
        let target = first_target(repository, targets).map_err(ChangesError::Git)?;
        let behind = match &target {
            Some(target) => {
                Some(behind(repository, &worktree.branch, target).map_err(ChangesError::Git)?)
            }
            None => None,
        };
        let (mergeable, mergeable_reason) = match &target {
            Some(target) => {
                match merge_trial(repository, target, &worktree.branch)
                    .map_err(ChangesError::Git)?
                {
                    Merge::Clean => (Some(true), None),
                    Merge::Conflicted(paths) => (Some(false), Some(conflict_sentence(&paths))),
                    Merge::Unasked(said) => (None, Some(said)),
                }
            }
            None => (None, Some(no_target_sentence(targets))),
        };
        let touches_ci = touches_ci(&files);
        Ok(Self {
            branch: worktree.branch.clone(),
            base_commit: base_commit.to_string(),
            head,
            target,
            ahead,
            behind,
            dirty,
            mergeable,
            mergeable_reason,
            files,
            touches_ci,
        })
    }
}

/// The four refusals both answers share, then the branch's tip: what a patch and a summary are both measured
/// from.
///
/// Extracted rather than copied, because "what is this session's diff base" must have one answer: the diff
/// endpoint's refusals are the summary's, byte for byte (ADR-0062), and the order is the reason a pruned
/// worktree whose directory is back answers `409` rather than a `500` from a read inside it.
///
/// 1. the repository is there (a project checkout that has gone away);
/// 2. the worktree directory is there (a `rm -rf`, or a teardown's leftovers);
/// 3. git registers it as a worktree of the repository (a `git worktree prune`);
/// 4. the branch is there, and its tip is what the patch is measured to.
fn basis(worktree: &Worktree) -> Result<String, ChangesError> {
    if !worktree.repository.is_dir() {
        return Err(ChangesError::NoRepository {
            repository: worktree.repository.clone(),
        });
    }
    if !worktree.path.is_dir() {
        return Err(ChangesError::Missing {
            path: worktree.path.clone(),
        });
    }
    match worktree.registered() {
        Ok(true) => {}
        Ok(false) => {
            return Err(ChangesError::Unregistered {
                path: worktree.path.clone(),
            });
        }
        Err(error) => return Err(ChangesError::Git(error)),
    }
    match worktree.branch_head() {
        Ok(Some(head)) => Ok(head),
        Ok(None) => Err(ChangesError::NoBranch {
            branch: worktree.branch.clone(),
        }),
        Err(error) => Err(ChangesError::Git(error)),
    }
}

/// Why the changes view has nothing to answer — one variant per fact an operator can act on.
///
/// The handler answers every one of these as `409 no_worktree` with its own sentence, because the four facts
/// are one thing to a client ("there is nothing to review") and four different next steps to the operator.
/// The variant carries the path or the name for the log line, which is the one place a path belongs: the
/// envelope's messages are fixed sentences (ADR-0008).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesError {
    /// `<projects>/<project>` is not a directory: the project's checkout has gone away.
    NoRepository { repository: PathBuf },
    /// The worktree directory is not there, though the row says it was made.
    Missing { path: PathBuf },
    /// The directory is there and git does not list it as a worktree of the repository (a `worktree prune`).
    Unregistered { path: PathBuf },
    /// `refs/heads/agent/<id>` is not in the repository.
    NoBranch { branch: String },
    /// Git itself could not be run, or answered something that is not an answer.
    Git(GitError),
}

impl ChangesError {
    /// The sentence an operator reads in the log, with the path the envelope is not allowed to carry.
    pub fn sentence(&self) -> String {
        match self {
            Self::NoRepository { repository } => {
                format!(
                    "the project repository {} is not there",
                    repository.display()
                )
            }
            Self::Missing { path } => format!("the worktree {} is not there", path.display()),
            Self::Unregistered { path } => {
                format!("git does not register {} as a worktree", path.display())
            }
            Self::NoBranch { branch } => format!("the branch {branch} is not there"),
            Self::Git(error) => error.to_string(),
        }
    }
}

/// One patch, as much of it as [`DIFF_CAP`] holds.
///
/// `body` is bytes rather than a `String` because a patch is a *file's* text, not ours: git prints a file's own
/// bytes, so a Latin-1 file's diff is not UTF-8, and pretending otherwise would either corrupt it or make the
/// answer's `charset` a lie. `total` is how many bytes git printed — exact unless `complete` was false, which
/// means the counting ceiling was reached and `total` is a lower bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// The patch, cut to fit [`DIFF_CAP`] with a marker on its last line when it did not fit.
    pub body: Vec<u8>,
    /// Whether anything was withheld: `body` is then a prefix of git's output plus one marker line.
    pub truncated: bool,
    /// How many bytes git printed, or — when the diff passed [`COUNT_MAX`] — a lower bound on them.
    pub total: u64,
    /// Whether `body` is valid UTF-8, which is the only thing that lets the answer claim `charset=utf-8`.
    pub utf8: bool,
}

/// Why the diff has nothing to answer: the summary's own facts, or a `path` that is not in the diff.
///
/// Two arms and not five, because the four `no_worktree` sentences and the `500` are the summary's: a client
/// that already reads `GET …/changes` learns nothing new. The new fact is the `path`, and it is a `404` — the
/// resource a client asked for does not exist — rather than a `422`: the parameter is well-formed, it just
/// names a file this session did not change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    /// Nothing to read, or git could not be asked — [`ChangesError`]'s own two families.
    Changes(ChangesError),
    /// `path` is not one of the changed paths: an unchanged file, an absolute path, a traversal.
    NotChanged { path: String },
}

impl From<ChangesError> for DiffError {
    fn from(error: ChangesError) -> Self {
        Self::Changes(error)
    }
}

impl From<GitError> for DiffError {
    fn from(error: GitError) -> Self {
        Self::Changes(ChangesError::Git(error))
    }
}

impl DiffError {
    /// The sentence an operator reads in the log, with the path the envelope is not allowed to carry.
    pub fn sentence(&self) -> String {
        match self {
            Self::Changes(error) => error.sentence(),
            Self::NotChanged { path } => {
                format!("{path} is not one of the paths this session changed")
            }
        }
    }
}

/// Answer the unified diff for one session's worktree: the whole branch, or one changed path in it.
///
/// The patch is `<base_commit>..<head>`, the **same two-dot basis** the summary's `files[]` is measured on
/// (ADR-0061's D2), so the index a client picks a path out of is the index this function validates against.
/// Two consequences are worth stating because they are not obvious:
///
/// - **It is commits only.** Uncommitted work is `dirty`'s fact on the summary, and the WIP snapshot
///   (`refs/holosmith/wip/<id>`, ADR-0049) is how a reader sees the working tree. A diff that mixed the two
///   would answer a question nobody asked ("what is in `/work`?") with a patch named after two commits.
/// - **`path` is a member of the list, not a path.** It is never joined to a filesystem path or handed to git
///   until the membership test has passed, so `..`, an absolute path and a spelled-around traversal are all the
///   same answer ("not a path this session changed"), and no traversal needs cleaning up because none is ever
///   built.
///
/// `None` is the whole branch, capped at [`DIFF_CAP`] like a single file's patch — the demo's "the whole branch"
/// and a 2 MB file are the same size question.
pub fn diff(worktree: &Worktree, base_commit: &str, path: Option<&str>) -> Result<Diff, DiffError> {
    let head = basis(worktree)?;
    let repository = worktree.repository.as_path();
    if let Some(path) = path {
        let changed = files(repository, base_commit, &head)?;
        if !changed.iter().any(|file| file.path == path) {
            return Err(DiffError::NotChanged {
                path: path.to_string(),
            });
        }
    }
    let args = patch_arguments(base_commit, &head, path);
    let patch = run_patch(repository, &args)?;
    Ok(assemble(patch))
}

/// The patch's argument vector: the three flags that make an answer the command's, `--no-color`, the two
/// commits, and `--` before a path so a name that begins with `-` can never be read as an option.
///
/// Split out so the vector can be asserted without running git. The vector is not the only proof that
/// `--no-ext-diff --no-textconv --no-color` are passed — a *repository-local* config can be written by a test
/// (`tests/changes.rs`'s three `a_repository_config_cannot_*` tests do exactly that, and each goes red when its
/// flag is dropped) — but it is the proof that survives a config a test cannot write, and it says in one place
/// what the command line is.
///
/// **`--literal-pathspecs` leads the vector** because the member is a *name*, not a pattern. `files[]` is a list
/// of exact strings and the membership test is an equality, but a git pathspec is a glob by default: a changed
/// file literally called `a*.md` would hand git `a*.md` and get the union of every matching path back, and a
/// leading `:` would be read as pathspec magic. A git *global* option, so it comes before `diff`.
fn patch_arguments(base: &str, head: &str, path: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec!["--literal-pathspecs".to_string(), "diff".to_string()];
    args.extend(DIFF_FLAGS.iter().map(|flag| (*flag).to_string()));
    args.push(NO_COLOR.to_string());
    args.push(base.to_string());
    args.push(head.to_string());
    if let Some(path) = path {
        args.push("--".to_string());
        args.push(path.to_string());
    }
    args
}

/// What one `git diff` read produced: the bytes kept, how many git printed, and whether the count is exact.
#[derive(Clone)]
struct Patch {
    head: Vec<u8>,
    total: usize,
    complete: bool,
}

/// Run the patch under [`DIFF_CAP`] and [`COUNT_MAX`], killing the child if it would exceed the count.
///
/// The read is **bounded, not buffered**: a 2 MB diff is never held in memory to be cut to 256 KB. At most
/// [`DIFF_CAP`] bytes are kept and at most [`COUNT_MAX`] counted; past the ceiling the child is killed and
/// `complete` is false, which the marker turns into "at least N bytes" rather than a number it did not
/// measure.
///
/// stderr is drained on its own thread, so a git that says a great deal cannot fill its pipe and deadlock
/// against our reading of stdout. A non-zero exit **before** the ceiling is a failure to ask — [`GitError`],
/// the `500` — and never an empty `200`; an exit status observed after a kill is not a verdict, because the
/// ceiling is ours.
fn run_patch(repository: &Path, args: &[String]) -> Result<Patch, GitError> {
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let command = git_command(repository, &borrowed);
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(&borrowed)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| GitError {
            command: command.clone(),
            detail: format!("git is not runnable ({error}): install git 2.38 or newer"),
        })?;
    let mut stdout = child.stdout.take().ok_or_else(|| GitError {
        command: command.clone(),
        detail: "git's output could not be read".to_string(),
    })?;
    let mut stderr = child.stderr.take();
    let drained = std::thread::spawn(move || {
        let mut said = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_end(&mut said);
        }
        said
    });
    let mut head: Vec<u8> = Vec::new();
    let mut total = 0usize;
    let mut complete = true;
    let mut chunk = vec![0u8; CHUNK];
    loop {
        if total >= COUNT_MAX {
            complete = false;
            let _ = child.kill();
            break;
        }
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                total += read;
                let room = DIFF_CAP.saturating_sub(head.len());
                head.extend_from_slice(&chunk[..read.min(room)]);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError {
                    command,
                    detail: format!("git's output could not be read ({error})"),
                });
            }
        }
    }
    let status = child.wait().map_err(|error| GitError {
        command: command.clone(),
        detail: format!("git could not be waited on ({error})"),
    })?;
    let said = drained.join().unwrap_or_default();
    if complete && !status.success() {
        return Err(git_error_from(command, &said, status));
    }
    Ok(Patch {
        head,
        total,
        complete,
    })
}

/// Turn what git printed into the answer's body: a short patch untouched, a long one cut at a line boundary
/// with a marker on its last line.
///
/// The cut lands on a **line boundary** because a diff is line-oriented: half a hunk line would be a lie about
/// content the reader is looking at. It then lands on a UTF-8 boundary **only when the cut fell mid-sequence of
/// an otherwise UTF-8 patch** (the error's `error_len()` is `None`); a patch that really is not UTF-8 keeps its
/// bytes and answers without a `charset` claim.
///
/// The marker's own length is inside the cap, so `body.len() <= DIFF_CAP` for every answer. It names the
/// **total**, never "showed N": the number shown would depend on the cut, the cut would depend on the marker's
/// length, and that circle would either make the marker a digit short or a promise broken. What was shown is
/// whatever the body holds above the marker line.
fn assemble(patch: Patch) -> Diff {
    let total = patch.total as u64;
    let truncated = patch.total > patch.head.len();
    if !truncated {
        let utf8 = std::str::from_utf8(&patch.head).is_ok();
        return Diff {
            body: patch.head,
            truncated,
            total,
            utf8,
        };
    }
    let marker = truncated_marker(total, patch.complete);
    let budget = DIFF_CAP.saturating_sub(marker.len() + 2);
    let mut kept = &patch.head[..budget.min(patch.head.len())];
    if let Some(end) = kept.iter().rposition(|byte| *byte == b'\n') {
        kept = &kept[..=end];
    }
    let mut utf8 = true;
    if let Err(error) = std::str::from_utf8(kept) {
        match error.error_len() {
            // The cut fell inside a character: give back the incomplete sequence, and the rest is UTF-8.
            None => kept = &kept[..error.valid_up_to()],
            // These bytes are not UTF-8, so nothing is given back and the answer says so.
            Some(_) => utf8 = false,
        }
    }
    let mut body = kept.to_vec();
    if !body.last().is_some_and(|byte| *byte == b'\n') {
        body.push(b'\n');
    }
    body.extend_from_slice(marker.as_bytes());
    body.push(b'\n');
    Diff {
        body,
        truncated: true,
        total,
        utf8,
    }
}

/// The line a truncated patch ends with: how big git's output was, and that this is not all of it.
///
/// Two sentences, because the two facts are different. An exact total is a measurement; "at least" is what the
/// ceiling earns, because the loop stops *at* [`COUNT_MAX`] without asking whether anything follows — so a diff
/// that is exactly the ceiling is reported as "at least", not as "longer than", which would be a claim about one
/// byte nobody read.
fn truncated_marker(total: u64, complete: bool) -> String {
    if complete {
        format!("[diff truncated: the diff is {total} bytes; only the first part is shown]")
    } else {
        format!(
            "[diff truncated: the diff is at least {total} bytes; only the first part is shown]"
        )
    }
}

/// What `merge-tree --write-tree` answered (item 12, `doctor`'s V2 trial is the same command).
///
/// Three arms, not two, and the third is why the JSON field can be `null`: a merge that *conflicts* is a
/// measurement (`false`), while a question that could not be asked is not.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Merge {
    /// Exit 0: git wrote a tree, so the merge would apply.
    Clean,
    /// Exit 1: the merge conflicts, and these are the paths (`<mode> <oid> <stage>\t<path>` lines).
    Conflicted(Vec<String>),
    /// Anything else — a target that is not a commit, a git without `--write-tree`, a killed process — with
    /// git's own words.
    Unasked(String),
}

/// Every changed path between two commits: `diff --numstat` for the counts, `diff --name-status` for the
/// status, joined by path.
///
/// **Two commands, and they cannot disagree**, because both read the same two commit ids: the objects are
/// content-addressed, so nothing an agent does in `/work` can change what they say between one call and the
/// next. That is why this is not one command (git does not print `--numstat` and `--name-status` together —
/// measured, the name-status wins) and why no lock is needed.
///
/// A path present in one read and not the other cannot happen for the same two ids; if it did, the file would
/// be listed with the status git gave it and no counts, which is the same answer a binary file gets.
fn files(repository: &Path, base: &str, head: &str) -> Result<Vec<FileChange>, GitError> {
    let counts = numstat(repository, base, head)?;
    let statuses = name_status(repository, base, head)?;
    Ok(statuses
        .into_iter()
        .map(|(path, status)| {
            let (additions, deletions) = counts
                .iter()
                .find(|(counted, _, _)| *counted == path)
                .map(|(_, additions, deletions)| (*additions, *deletions))
                .unwrap_or((None, None));
            FileChange {
                path,
                status,
                additions,
                deletions,
            }
        })
        .collect())
}

/// One `--numstat` row: the path, then the two counts (`None` where git printed `-`).
type Counted = (String, Option<u64>, Option<u64>);

/// `git diff --numstat -z`: `<additions>\t<deletions>\t<path>\0` per file, `-` for a count git cannot make.
fn numstat(repository: &Path, base: &str, head: &str) -> Result<Vec<Counted>, GitError> {
    let args = args(["diff", "--numstat", "-z"], base, head);
    let output = git_output(repository, &args)?;
    if !output.status.success() {
        return Err(git_error(repository, &args, &output));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .filter_map(parse_numstat)
        .collect())
}

/// One `--numstat -z` record: two counts and a path, or `None` when git printed something else.
fn parse_numstat(record: &[u8]) -> Option<Counted> {
    let mut fields = record.splitn(3, |byte| *byte == b'\t');
    let additions = count(fields.next()?)?;
    let deletions = count(fields.next()?)?;
    let path = String::from_utf8_lossy(fields.next()?).into_owned();
    Some((path, additions, deletions))
}

/// A numstat count: a number, or `None` for `-` (git's word for a file it cannot count lines in).
///
/// `None` rather than an error twice over: a binary file is not a malformed record, and a count git printed
/// as something unparseable is better reported as "no count" than as a failed request over one path.
fn count(field: &[u8]) -> Option<Option<u64>> {
    let text = std::str::from_utf8(field).ok()?;
    if text == "-" {
        return Some(None);
    }
    Some(text.parse().ok())
}

/// `git diff --name-status -z`: `<status>\0<path>\0` per file, or `--no-renames` makes it so.
fn name_status(
    repository: &Path,
    base: &str,
    head: &str,
) -> Result<Vec<(String, String)>, GitError> {
    let args = args(["diff", "--name-status", "-z"], base, head);
    let output = git_output(repository, &args)?;
    if !output.status.success() {
        return Err(git_error(repository, &args, &output));
    }
    let mut fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned());
    let mut files = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        files.push((path, status));
    }
    Ok(files)
}

/// The two diff reads' argument list: the flags, then the two commits.
fn args<'a>(command: [&'a str; 3], base: &'a str, head: &'a str) -> Vec<&'a str> {
    let mut all = command.to_vec();
    all.extend_from_slice(&DIFF_FLAGS);
    all.push(base);
    all.push(head);
    all
}

/// Whether the worktree holds uncommitted work: any line of `git status --porcelain`.
///
/// The flags are the whole decision. `--untracked-files=normal` is passed rather than inherited, because
/// `status.showUntrackedFiles = no` in a repository's config would make an agent's new file invisible and
/// `dirty: false` would then be a lie about work that exists; `--no-optional-locks` keeps git from refreshing
/// and rewriting the index, which is what makes this read safe while the agent is committing in the same
/// worktree (an `index.lock` held by somebody else is otherwise a failed request).
///
/// Ignored files do not count: `status` does not list them without `--ignored`, and they are not work — the
/// WIP snapshot's `add -A` would not store them either.
fn dirty(worktree: &Path) -> Result<bool, GitError> {
    let args = [
        "--no-optional-locks",
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ];
    let output = git_output(worktree, &args)?;
    if !output.status.success() {
        return Err(git_error(worktree, &args, &output));
    }
    Ok(!output.stdout.iter().all(u8::is_ascii_whitespace))
}

/// The first branch of `targets` this repository has, or `None` when it has none of them.
///
/// `[merge] targets` defaults to `["main", "master"]` because a machine has one of them (HS-043), and the
/// order is the answer: "the UI preselects the first" is this function. A name that is not there is not a
/// target — the same reading the delete's keep-rule makes when it asks whether a branch is merged.
fn first_target(repository: &Path, targets: &[String]) -> Result<Option<String>, GitError> {
    for target in targets {
        let reference = format!("refs/heads/{target}");
        let args = ["rev-parse", "--verify", "--quiet", reference.as_str()];
        let output = git_output(repository, &args)?;
        match output.status.code() {
            Some(0) => return Ok(Some(target.clone())),
            Some(1) => continue,
            _ => return Err(git_error(repository, &args, &output)),
        }
    }
    Ok(None)
}

/// How many commits the target has that the branch does not: `rev-list --count <branch>..<target>`.
///
/// The other half of [`Worktree::ahead`], and the spec's wording in as many words: "behind counts commits on
/// the target that the branch lacks".
fn behind(repository: &Path, branch: &str, target: &str) -> Result<u64, GitError> {
    let range = format!("refs/heads/{branch}..refs/heads/{target}");
    let args = ["rev-list", "--count", range.as_str()];
    let output = git_output(repository, &args)?;
    if !output.status.success() {
        return Err(git_error(repository, &args, &output));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    text.parse().map_err(|_| GitError {
        command: git_command(repository, &args),
        detail: format!("git printed {text:?}, which is not a count"),
    })
}

/// The `merge-tree` argument vector: `--write-tree`, then the target, then the branch, both fully qualified.
///
/// Split out so the order can be asserted without running git (see the doc on [`merge_trial`]).
fn merge_arguments(target: &str, branch: &str) -> [String; 4] {
    [
        "merge-tree".to_string(),
        "--write-tree".to_string(),
        format!("refs/heads/{target}"),
        format!("refs/heads/{branch}"),
    ]
}

/// Ask `merge-tree --write-tree` whether `branch` would merge into `target`, touching no working tree.
///
/// The refs are fully qualified so that a tag named `main` cannot be merged instead of the branch, and the
/// order is the merge's: `merge-tree <target> <branch>` is "merge the session branch into the target", which
/// is what HS-054 will run. The order is pinned by [`tests::the_merge_trial_asks_for_the_target_first`],
/// because a conflict is symmetric — `merge-tree A B` and `merge-tree B A` find the same paths — so no
/// fixture can tell the two apart even though the sentence a conflict produces would name the wrong side.
///
/// Exit 0 is a clean merge and exactly one line (the tree's id); exit 1 is a conflict, and its stdout carries
/// the tree's id followed by `<mode> <oid> <stage>\t<path>` lines, then a blank line and git's prose —
/// measured on 2.43, and the same two shapes `doctor`'s V2 trial asserts. Anything else is a question that
/// could not be asked, which is `None` in the JSON rather than `false`.
fn merge_trial(repository: &Path, target: &str, branch: &str) -> Result<Merge, GitError> {
    let args = merge_arguments(target, branch);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = git_output(repository, &refs)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match output.status.code() {
        Some(0) => Ok(Merge::Clean),
        Some(1) => Ok(Merge::Conflicted(conflict_paths(&stdout))),
        code => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let said = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            let word = match code {
                Some(code) => format!("answered {code}"),
                None => "was killed by a signal".to_string(),
            };
            Ok(Merge::Unasked(format!(
                "`git merge-tree --write-tree` {word}, so the merge question could not be asked: {said}"
            )))
        }
    }
}

/// The paths a conflict names, from `merge-tree --write-tree`'s exit-1 stdout.
///
/// The first line is the tree git wrote (it writes one even for a conflict), and each conflicted path then
/// appears one or more times — once per stage — as `<mode> <oid> <stage>\t<path>`. The blank line and the
/// prose that follow are git talking to a human, so the walk stops at the first line without a tab.
fn conflict_paths(stdout: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for line in stdout.lines().skip(1) {
        let Some((_, path)) = line.split_once('\t') else {
            break;
        };
        if !paths.iter().any(|seen| seen == path) {
            paths.push(path.to_string());
        }
    }
    paths
}

/// The sentence a conflict earns: the paths, because that is what the operator acts on.
fn conflict_sentence(paths: &[String]) -> String {
    if paths.is_empty() {
        // A conflict git reported without a path is still a conflict; saying nothing would leave a `false`
        // with no reason, which is the one shape this field exists to avoid.
        return "the merge conflicts, but git named no path it could not merge".to_string();
    }
    format!("the merge conflicts in: {}", paths.join(", "))
}

/// The sentence a repository with no target earns.
fn no_target_sentence(targets: &[String]) -> String {
    if targets.is_empty() {
        return "no merge targets are configured for this machine, so there is nothing to merge into"
            .to_string();
    }
    format!(
        "this project has none of the configured merge targets ({}), so there is nothing to merge into",
        targets.join(", ")
    )
}

/// Whether the diff changes CI configuration or a hook directory ([`CI_PATHS`]).
///
/// An **added** path counts like a modified one: a new workflow is exactly the thing the warning exists for.
/// The two spellings are **exclusive** — an entry that ends in `/` is a directory prefix compared against the
/// whole path, and every other entry is a file name compared against the last segment — so a root-level
/// `Jenkinsfile` counts and a `Jenkinsfile.old` beside it does not. Applying both arms to both kinds (the first
/// version of this function) made every *backup copy* of a CI file answer yes, which is a spurious warning
/// rather than a missed one but still a lie about the diff.
pub fn touches_ci(files: &[FileChange]) -> bool {
    files.iter().any(|file| {
        let path = file.path.as_str();
        let name = path.rsplit('/').next().unwrap_or(path);
        CI_PATHS.iter().any(|entry| {
            if entry.ends_with('/') {
                path.starts_with(entry)
            } else {
                name == *entry
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> FileChange {
        FileChange {
            path: path.to_string(),
            status: "M".to_string(),
            additions: Some(1),
            deletions: Some(0),
        }
    }

    #[test]
    fn the_ci_set_is_a_directory_or_a_name() {
        // The two spellings, one predicate. `.github/workflows/ci.yml` is the directory arm, a `Jenkinsfile`
        // under a subdirectory is the name arm, and `docs/github.md` is neither — the prefix is compared from
        // the start of the path, so a look-alike directory cannot answer yes.
        assert!(touches_ci(&[file(".github/workflows/ci.yml")]));
        assert!(touches_ci(&[file("ci/Jenkinsfile")]));
        assert!(touches_ci(&[file(".gitlab-ci.yml")]));
        assert!(!touches_ci(&[file("docs/github.md")]));
        assert!(!touches_ci(&[file("src/.github.md")]));
        assert!(!touches_ci(&[]));
    }

    #[test]
    fn an_added_ci_file_counts_like_a_modified_one() {
        // The story's own reading: a *new* workflow is the danger, so the status is not part of the question.
        let mut added = file(".github/workflows/release.yml");
        added.status = "A".to_string();
        assert!(touches_ci(&[added]));
    }

    #[test]
    fn the_workflow_prompts_are_not_ci() {
        // A path that merely starts with a `.github/`-look-alike must not count; this is the arm the prefix
        // comparison could get wrong by using `contains`.
        assert!(!touches_ci(&[file(".githubx/ci.yml")]));
        assert!(!touches_ci(&[file("src/Jenkinsfile.rs")]));
    }

    #[test]
    fn a_backup_of_a_ci_file_is_not_ci() {
        // The two arms are exclusive: a file-name entry is compared against the last segment, not used as a
        // prefix of the whole path. Before that fix a root-level `Jenkinsfile.old` answered `true` through
        // `path.starts_with("Jenkinsfile")`, and `src/Jenkinsfile.rs` was the only look-alike a test tried —
        // which passed because its *path* does not start at the entry. This is the arm that `src/` misses.
        assert!(!touches_ci(&[file("Jenkinsfile.old")]));
        assert!(!touches_ci(&[file("Jenkinsfile.rs")]));
        assert!(!touches_ci(&[file(".gitlab-ci.yml.bak")]));
        assert!(!touches_ci(&[file("codecov.yml.orig")]));
        // ... and the real ones still count, at the root and in a directory.
        assert!(touches_ci(&[file("Jenkinsfile")]));
        assert!(touches_ci(&[file("ci/Jenkinsfile")]));
    }

    #[test]
    fn the_merge_trial_asks_for_the_target_first() {
        // A conflict is symmetric — `merge-tree A B` and `merge-tree B A` name the same paths — so no fixture
        // can distinguish the order. The vector is pinned instead, because the order is what makes the sentence
        // "the merge conflicts in …" about merging the branch *into the target* (HS-054's own direction).
        assert_eq!(
            merge_arguments("main", "agent/0123456789abcdef"),
            [
                "merge-tree".to_string(),
                "--write-tree".to_string(),
                "refs/heads/main".to_string(),
                "refs/heads/agent/0123456789abcdef".to_string(),
            ]
        );
    }

    #[test]
    fn a_conflict_is_read_from_the_stage_lines_and_stops_at_the_prose() {
        // The measured shape on git 2.43: the tree's id, one line per conflicted path per stage, a blank
        // line, then git's own sentences. A reader that took every line after the first would report
        // "Auto-merging f.txt" as a path.
        let stdout = "e6f917a5d20dfe90af6af093971520197f150c31\n\
                      100644 422c2b7ab3b3c668038da977e4e93a5fc623169c 1\tf.txt\n\
                      100644 729844c6cb7c9223f9d3a01d6529f4cd56bbdc17 2\tf.txt\n\
                      100644 de980441c3ab03a8c07dda1ad27b8a11f39deb1e 3\tf.txt\n\
                      \n\
                      Auto-merging f.txt\n\
                      CONFLICT (content): Merge conflict in f.txt\n";
        assert_eq!(conflict_paths(stdout), vec!["f.txt".to_string()]);
        assert_eq!(
            conflict_paths("abc\n100644 1 1\ta.txt\n100644 2 1\tb.txt\n"),
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
    }

    #[test]
    fn a_numstat_record_is_two_counts_and_a_path() {
        assert_eq!(
            parse_numstat(b"3\t0\ta.txt"),
            Some(("a.txt".to_string(), Some(3), Some(0)))
        );
        // A binary file: `-`/`-`, which is `None` rather than `0` — "no lines counted", not "no lines".
        assert_eq!(
            parse_numstat(b"-\t-\timage.png"),
            Some(("image.png".to_string(), None, None))
        );
    }

    #[test]
    fn the_diff_reads_ask_for_the_command_and_not_the_repository_s_config() {
        // Three flags are the rule, and each answers one config key: `diff.external`, a `.gitattributes`
        // textconv filter, and rename detection's `{old => new}` spelling. The first two are also measured
        // behaviourally for the *patch* (`tests/changes.rs`'s `a_repository_config_cannot_*`), but these two
        // reads are joined by path, so the spelling is pinned here as well — the same reason `worktree.rs`
        // pins `CONFIG_READ`.
        assert!(DIFF_FLAGS.contains(&"--no-ext-diff"));
        assert!(DIFF_FLAGS.contains(&"--no-textconv"));
        assert!(DIFF_FLAGS.contains(&"--no-renames"));
    }

    #[test]
    fn the_diff_reads_end_with_the_two_commits_it_names() {
        let args = args(["diff", "--numstat", "-z"], "aaa", "bbb");
        assert_eq!(
            args,
            [
                "diff",
                "--numstat",
                "-z",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "aaa",
                "bbb"
            ]
        );
    }

    /// A patch read with `total` bytes of which `head` is what was kept.
    fn patch(head: &[u8], total: usize) -> Patch {
        Patch {
            head: head.to_vec(),
            total,
            complete: true,
        }
    }

    #[test]
    fn the_patch_vector_pins_the_flags_the_no_color_and_the_separator() {
        // The acceptance's third bullet, pinned: `tests/changes.rs` measures all three flags against a
        // repository config that would undo them, and this pins the vector itself, so a flag added or dropped
        // here is a diff a reviewer sees rather than a behaviour nobody checked. `--` before the path is the
        // reason a name that begins with `-` can never be read as an option.
        assert_eq!(
            patch_arguments("aaa", "bbb", Some("a.txt")),
            [
                "--literal-pathspecs",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--no-color",
                "aaa",
                "bbb",
                "--",
                "a.txt"
            ]
            .map(str::to_string)
        );
    }

    #[test]
    fn the_whole_branch_patch_names_no_path_and_no_separator() {
        // A bare `--` with no path after it is accepted by git but reads like a mistake; the vector without a
        // path is the two commits and nothing else.
        assert_eq!(
            patch_arguments("aaa", "bbb", None),
            [
                "--literal-pathspecs",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--no-color",
                "aaa",
                "bbb"
            ]
            .map(str::to_string)
        );
    }

    #[test]
    fn a_short_patch_is_the_command_s_own_bytes() {
        let bytes = b"diff --git a/a.txt b/a.txt\n+one\n";
        let diff = assemble(patch(bytes, bytes.len()));
        assert!(!diff.truncated);
        assert_eq!(diff.body, bytes.to_vec());
        assert_eq!(diff.total, bytes.len() as u64);
        assert!(diff.utf8);
    }

    #[test]
    fn a_patch_that_is_not_utf8_keeps_its_bytes_and_claims_no_charset() {
        // git prints a *file's* bytes: a Latin-1 file's diff has 0xe9 in it, and a `charset=utf-8` claim or a
        // lossy conversion would both be lies about text the operator asked to read.
        let bytes = b"diff --git a/x b/x\n+caf\xe9\n";
        let diff = assemble(patch(bytes, bytes.len()));
        assert_eq!(diff.body, bytes.to_vec());
        assert!(!diff.utf8, "0xe9 alone is not UTF-8");
    }

    #[test]
    fn an_over_cap_patch_is_cut_at_a_line_boundary_and_names_the_total() {
        let mut bytes = Vec::new();
        while bytes.len() <= DIFF_CAP * 2 {
            bytes.extend_from_slice(b"+a line of the diff\n");
        }
        let total = bytes.len();
        let diff = assemble(patch(&bytes[..DIFF_CAP], total));
        assert!(diff.truncated);
        assert_eq!(diff.total, total as u64);
        assert!(
            diff.body.len() <= DIFF_CAP,
            "the whole response, marker included, is capped: {} bytes",
            diff.body.len()
        );
        let text = String::from_utf8(diff.body.clone()).expect("the kept prefix is UTF-8");
        let marker = text.lines().last().expect("a last line");
        assert!(marker.starts_with("[diff truncated:"), "{marker}");
        assert!(
            marker.contains(&total.to_string()),
            "the marker names the total, not a guess: {marker}"
        );
        let kept = &text[..text.len() - marker.len() - 1];
        assert!(kept.ends_with('\n'), "the cut is not a line boundary");
        assert!(
            bytes.starts_with(kept.as_bytes()),
            "the body is not a prefix of what git printed"
        );
    }

    #[test]
    fn a_cut_inside_a_character_gives_the_incomplete_sequence_back() {
        // One line with no newline at all, so the line boundary cannot help: the cut lands wherever the budget
        // falls, and if that is inside a character the body would not be UTF-8. The floor fixes it, and the
        // charset claim survives — which is the difference between this and a file that is really not UTF-8.
        //
        // The test also asserts that the raw cut **is** mid-character, because only that makes it a test of the
        // floor: the parity depends on the marker's length, so a one-byte change to the marker's wording could
        // land the cut on a character boundary and leave this green with the floor deleted. Asserting the
        // premise turns that into a failure whose sentence says what to do about it.
        let mut bytes = b"+".to_vec();
        bytes.extend_from_slice("é".repeat(DIFF_CAP).as_bytes());
        let total = bytes.len();
        let diff = assemble(patch(&bytes[..DIFF_CAP], total));
        assert!(diff.truncated);
        let text = String::from_utf8(diff.body.clone()).expect("the kept prefix is UTF-8");
        let marker = text.lines().last().expect("a last line");
        let budget = DIFF_CAP - marker.len() - 2;
        assert!(
            std::str::from_utf8(&bytes[..budget]).is_err(),
            "the fixture's cut is character-aligned, so this test proves nothing about the floor: make the \
             marker's wording a byte longer, or use a 3-byte character"
        );
        assert!(
            diff.utf8,
            "a mid-character cut does not make the text non-UTF-8"
        );
        assert!(diff.body.len() <= DIFF_CAP);
    }

    #[test]
    fn a_patch_past_the_counting_ceiling_says_at_least_rather_than_a_number() {
        // The ceiling is ours, not git's: past it the child is killed, so the total is a lower bound and the
        // marker must say so instead of printing a figure that stopped being the answer.
        let mut counted = Patch {
            head: vec![b'x'; DIFF_CAP],
            total: COUNT_MAX + CHUNK,
            complete: false,
        };
        let diff = assemble(counted.clone());
        let text = String::from_utf8(diff.body.clone()).expect("ASCII is UTF-8");
        let marker = text.lines().last().expect("a last line");
        assert!(marker.contains("at least"), "{marker}");
        assert!(marker.contains(&counted.total.to_string()), "{marker}");
        assert!(diff.body.len() <= DIFF_CAP);
        // The exact-total arm, for contrast: the same size but counted to the end.
        counted.complete = true;
        let exact = assemble(counted);
        let text = String::from_utf8(exact.body.clone()).expect("ASCII is UTF-8");
        let marker = text.lines().last().expect("a last line");
        assert!(!marker.contains("at least"), "{marker}");
        assert!(exact.body.len() <= DIFF_CAP, "the cap still holds");
    }
}
