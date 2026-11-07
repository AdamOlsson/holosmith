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
//! What this module does not do: no write, no fetch, no lock, and no diff. The unified diff is HS-053's,
//! the merge is HS-054's, and `touches_ci`'s *consumer* is HS-058's (which reads [`CI_PATHS`] through
//! [`touches_ci`], so the warning before a publish cannot become a second predicate).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::worktree::{GitError, Worktree, git_command, git_error, git_output};

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
        let head = match worktree.branch_head() {
            Ok(Some(head)) => head,
            Ok(None) => {
                return Err(ChangesError::NoBranch {
                    branch: worktree.branch.clone(),
                });
            }
            Err(error) => return Err(ChangesError::Git(error)),
        };
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

/// Ask `merge-tree --write-tree` whether `branch` would merge into `target`, touching no working tree.
///
/// The refs are fully qualified so that a tag named `main` cannot be merged instead of the branch, and the
/// order is the merge's: `merge-tree <target> <branch>` is "merge the session branch into the target", which
/// is what HS-054 will run.
///
/// Exit 0 is a clean merge and exactly one line (the tree's id); exit 1 is a conflict, and its stdout carries
/// the tree's id followed by `<mode> <oid> <stage>\t<path>` lines, then a blank line and git's prose —
/// measured on 2.43, and the same two shapes `doctor`'s V2 trial asserts. Anything else is a question that
/// could not be asked, which is `None` in the JSON rather than `false`.
fn merge_trial(repository: &Path, target: &str, branch: &str) -> Result<Merge, GitError> {
    let target_ref = format!("refs/heads/{target}");
    let branch_ref = format!("refs/heads/{branch}");
    let args = [
        "merge-tree",
        "--write-tree",
        target_ref.as_str(),
        branch_ref.as_str(),
    ];
    let output = git_output(repository, &args)?;
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
/// The rule is [`CI_PATHS`]'s two spellings — a directory prefix, or a file name — and both are applied to
/// the same changed path, so a `.github/workflows/ci.yml` addition and a `Jenkinsfile` at the repository's
/// root are one predicate.
pub fn touches_ci(files: &[FileChange]) -> bool {
    files.iter().any(|file| {
        let path = file.path.as_str();
        let name = path.rsplit('/').next().unwrap_or(path);
        CI_PATHS
            .iter()
            .any(|entry| path.starts_with(entry) || name == *entry)
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
        // textconv filter, and rename detection's `{old => new}` spelling. A behavioural test cannot set
        // those (the crate denies `unsafe`, so no test sets the process's git config), so the spelling is
        // pinned — the same reason `worktree.rs` pins `CONFIG_READ`.
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
}
