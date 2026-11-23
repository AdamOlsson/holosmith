//! The candidate: the one commit a promotion would make live.
//!
//! A promotion has to answer "which commit would this machine make live" before it can answer anything else, and
//! the answer is a fact about the **source tree** rather than about the request. `[update] source` names a
//! checkout; its `master`, checked out, is the commit; `git describe --tags --always` is the tag the release
//! directory will be named after (ADR-0110 D3); and the tree must be clean. [`resolve`] is the one function that
//! reads all of that, and [`Candidate`] is what it answers, so the page that draws a button, the request that
//! presses it and the job's report read **one spelling** of "what would be promoted". Two spellings is the defect
//! this module exists to prevent.
//!
//! Three things a candidate is not:
//!
//! * **Not an arbitrary ref.** There is no flag for a tag or a branch: the commit a release directory names has
//!   to be derivable from the tree it was built out of.
//! * **Not a worktree.** A session's `.worktrees/<id>` is a branch waiting to be merged, and it shares
//!   `refs/heads` with the repository it was made from — so `refs/heads/master` alone would answer there, while
//!   `status` and `describe` would be the worktree's. A checkout whose `HEAD` is not `refs/heads/master` is
//!   [`Refusal::NotOnMaster`], which is what makes a promotion that read one impossible rather than merely
//!   discouraged: it would make live code no merge and no review had touched.
//! * **Not a dirty tree.** A build from a tree holding files the commit does not have is one nobody can
//!   reproduce, so a tree that is not clean is [`Refusal::Dirty`] and not a candidate carrying a `-dirty` tag —
//!   and [`Candidate`] has no `clean` flag for the same reason: a value that could only ever be `true` is a check a
//!   caller can forget.
//!
//! # The reads, and why they are five commands
//!
//! Every one goes through [`crate::worktree`]'s helpers, so a failure's sentence is the tree's one spelling of
//! what git said, and every answer is an **exit code** rather than a reading of git's English:
//!
//! ```text
//! git -C <source> rev-parse --git-dir
//! git -C <source> symbolic-ref --quiet HEAD
//! git -C <source> rev-parse --verify --quiet refs/heads/master
//! git -C <source> --no-optional-locks status --porcelain -uall -z --no-renames
//! git -C <source> describe --tags --always
//! ```
//!
//! The first is the repository question: exit 0 inside one, 128 outside. The second is the checkout question,
//! and it is what makes the other three one commit: exit 0 prints the ref `HEAD` points at, which has to be
//! `refs/heads/master`, and exit 1 prints nothing at all, because `HEAD` is detached. The third is the commit:
//! its `--verify --quiet` exits 1, printing nothing, when there is no such ref, which — after the second read —
//! is an unborn `master` and not a missing branch. So "not a repository", "not on `master`", "`master` has no
//! commit" and "git failed to answer" are told apart by three commands and their exit codes, and nothing parses
//! a message. The third's ref is fully qualified so the answer is the branch's commit and not whichever kind of
//! object a bare `master` happens to name.
//!
//! The second is not a formality in front of the others: `status --porcelain` reports the index and the working
//! tree against **`HEAD`** and `describe` names **`HEAD`**'s commit, so a checkout left on a feature branch, on a
//! detached tag or on a session's `.worktrees/<id>` would answer `master`'s commit beside another tree's tag and
//! cleanliness — one value naming two commits, which is the defect this module exists to prevent.
//!
//! `-uall` on the fourth is load-bearing: `status --porcelain` respects `status.showUntrackedFiles=no`, so a
//! repository's own config would hide exactly the untracked file this check exists for. `-z` and `--no-renames`
//! keep one record per path: a NUL-terminated record carries a name with a non-ASCII byte raw rather than
//! `core.quotePath`'s octal escapes, and a rename is the two records it is instead of a second path with no
//! status columns in front of it. `--no-optional-locks` is the tree's habit on read-only git
//! ([`crate::changes`]): a read taken while something else commits must not fail on the index lock. Any record at
//! all is a dirty tree. This is not a second spelling of [`crate::changes`]' dirty flag: that one answers whether
//! a session's worktree has work for the diff sheet, in a bool and with `--untracked-files=normal`, while a
//! promotion needs the entries themselves so its refusal can name the file — which is what `-uall` prints.
//!
//! The fifth carries no `--dirty` because `clean` ran first: a candidate is only ever a commit, and
//! `scripts/build.sh` bakes `git describe --tags --always --dirty` into the artifact, so on the clean tree of
//! `master` the two answers are the same string.
//!
//! # What this module does not decide
//!
//! **The wire.** `404 no_candidate` and `422 stale_candidate` are the update routes' status and word; what this
//! module owes them is the distinguishable state ([`Refusal::NoCandidate`]) and the staleness predicate
//! ([`Candidate::stale`]).
//!
//! **The tag's rule.** [`crate::release::tag_problem`] says whether a tag can name a release directory, and
//! [`crate::promote`] applies it when it writes `releases/<tag>/`. This value carries `git describe`'s answer
//! unedited rather than papering a bad one over: a candidate whose tag the promotion would refuse is a fact the
//! report should state.
//!
//! # Two `Candidate`s
//!
//! `promote::Candidate` is the **artifact** a command read — its path, its bytes, its digest and its
//! `version --json` — and is private to that module. A caller outside `promote` sees this one only: the
//! source-tree commit a promotion is about.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::worktree::{git_error, git_output};

/// How many `git status --porcelain` entries a dirty tree's sentence prints before it counts the rest.
///
/// "Few" is deliberate: the operator needs enough to recognise which work interrupted a promotion and not a
/// listing they can get from the command the sentence names.
const DIRTY_ENTRIES_SHOWN: usize = 3;

const REPOSITORY: [&str; 2] = ["rev-parse", "--git-dir"];

/// The one ref a candidate is on, fully qualified so the answer is the branch's commit rather than whichever
/// kind of object a bare `master` happens to name.
const MASTER_REF: &str = "refs/heads/master";

/// The checkout question: exit 0 prints the ref `HEAD` points at, exit 1 prints nothing because it is detached.
const HEAD_REF: [&str; 3] = ["symbolic-ref", "--quiet", "HEAD"];

const MASTER: [&str; 4] = ["rev-parse", "--verify", "--quiet", MASTER_REF];

const STATUS: [&str; 6] = [
    "--no-optional-locks",
    "status",
    "--porcelain",
    "-uall",
    "-z",
    "--no-renames",
];

const DESCRIBE: [&str; 3] = ["describe", "--tags", "--always"];

/// The commit a promotion would make live: a clean checkout's `master`, and the tag it describes itself with.
///
/// [`resolve`] is its only constructor. The fields are `pub` because that is how this crate's values are read, so
/// the rule is a doc and not a compiler error: a `Candidate` built from a caller's own `rev-parse` is the second
/// spelling of "what would be promoted" this module exists to prevent, and is a bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The checkout `[update] source` names.
    pub source: PathBuf,
    /// `master`'s commit, as `git rev-parse` prints it.
    pub commit: String,
    /// `git describe --tags --always`'s answer: the name `releases/<tag>/` will get.
    ///
    /// It is carried unedited. Whether it can be a directory's name is
    /// [`crate::release::tag_problem`]'s rule, and the promotion is what applies it.
    pub tag: String,
}

impl Candidate {
    /// The staleness check: whether `commit` is still `master`, and if not, the refusal naming both.
    ///
    /// The commit a request carries is an **optimistic-concurrency token and not an acknowledgement**: the page
    /// rendered this value, so a press that arrives after `master` moved is a press about a tree nobody has seen.
    /// `None` means the request's commit is this candidate's; `Some` is the refusal a route turns into
    /// `422 stale_candidate`.
    ///
    /// Nothing on this module's own path calls it. The routes do — the `GET` renders [`Candidate::commit`] and
    /// the `POST` carries it back — and it is one function for both so no caller can word the comparison twice.
    pub fn stale(&self, commit: &str) -> Option<Stale> {
        (commit != self.commit).then(|| Stale {
            requested: commit.to_string(),
            current: self.commit.clone(),
        })
    }
}

/// The commit a request carried, refused because `master` has moved since its page was rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    /// The commit the request named: what its page rendered.
    pub requested: String,
    /// The commit `master` names now: the candidate's own.
    pub current: String,
}

impl fmt::Display for Stale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the request names commit {}, and `master` is {} now: the page was rendered from a tree that \
             moved, so reload it and press promote again",
            self.requested, self.current
        )
    }
}

/// Why this machine has no candidate to name.
///
/// One state per fact, so a route can turn [`Refusal::NoCandidate`] into its `404` and the rest into the reason
/// the source tree is not promotable. Every sentence names the repair — the one command, or the words git itself
/// gave when the failure was git's rather than the checkout's — because an operator reading a page has to be able
/// to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `[update] source` is unset: this machine has no candidate at all.
    NoCandidate,
    /// There is no checkout git can read at the path: it is not a repository, or git could not answer.
    NotARepository { source: PathBuf, detail: String },
    /// The checkout's `HEAD` is not `master`: another branch is checked out, or none at all.
    NotOnMaster {
        source: PathBuf,
        /// The ref `HEAD` points at, or `None` when the checkout is detached.
        head: Option<String>,
    },
    /// `master` exists as a name and has no commit behind it, so there is nothing to promote.
    NoMaster { source: PathBuf, detail: String },
    /// The tree holds work the commit does not: the first few `status --porcelain` entries.
    Dirty {
        source: PathBuf,
        entries: Vec<String>,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCandidate => f.write_str(
                "this machine has no candidate: a candidate is the `master` of a clean checkout, named by \
                 `[update] source`, and it is what a promotion would make live. Point that key at the checkout \
                 with `holosmith config edit`",
            ),
            Self::NotARepository { source, detail } => {
                write!(
                    f,
                    "`[update] source` is {}, and there is no checkout git can read there ({detail}): a \
                     candidate is the `master` of one, so make one beside that path with `git clone <url> \
                     {}-src`",
                    source.display(),
                    source.display()
                )
            }
            Self::NotOnMaster { source, head } => {
                write!(
                    f,
                    "{} does not have `master` checked out ({}), so a promotion would build whatever its \
                     `HEAD` names rather than the commit it reports: check it out there with `git -C {} \
                     checkout master`",
                    source.display(),
                    head_state(head),
                    source.display()
                )
            }
            Self::NoMaster { source, detail } => {
                write!(
                    f,
                    "{} has no commit on `master` ({detail}), so there is nothing for a promotion to make \
                     live: a candidate is the `master` of a checkout with a release line, so point \
                     `[update] source` at that one with `holosmith config edit`",
                    source.display()
                )
            }
            Self::Dirty { source, entries } => {
                write!(
                    f,
                    "{} is not clean, so a build would carry files `master` does not have ({}): put them aside \
                     with `git -C {} stash --include-untracked`",
                    source.display(),
                    shown(entries),
                    source.display()
                )
            }
        }
    }
}

/// The `HEAD` state as the sentence names it: the ref git printed, or that the checkout is detached.
fn head_state(head: &Option<String>) -> String {
    match head {
        Some(reference) => format!("`HEAD` is on `{reference}`"),
        None => "`HEAD` is detached".to_string(),
    }
}

/// The dirty refusal's list: the first [`DIRTY_ENTRIES_SHOWN`] entries, then how many were left out.
fn shown(entries: &[String]) -> String {
    let listed: Vec<&str> = entries
        .iter()
        .take(DIRTY_ENTRIES_SHOWN)
        .map(String::as_str)
        .collect();
    let rest = entries.len().saturating_sub(listed.len());
    match rest {
        0 => listed.join(", "),
        rest => format!("{}, and {rest} more", listed.join(", ")),
    }
}

/// The candidate `config` names, or why this machine has none.
///
/// One function for every reader: the page that draws the promotion button, the request that presses it and the
/// job's report all answer "what would be promoted" here, so a commit that landed while the page was open cannot
/// be promoted by accident and cannot be described two ways.
pub fn resolve(config: &Config) -> Result<Candidate, Refusal> {
    let Some(source) = config.update.source.clone() else {
        return Err(Refusal::NoCandidate);
    };
    repository(&source)?;
    checked_out(&source)?;
    let commit = master(&source)?;
    clean(&source)?;
    let tag = describe(&source)?;
    Ok(Candidate {
        source,
        commit,
        tag,
    })
}

/// The repository question: exit 0 inside one, anything else outside.
fn repository(source: &Path) -> Result<(), Refusal> {
    let output = git_output(source, &REPOSITORY).map_err(|error| Refusal::NotARepository {
        source: source.to_path_buf(),
        detail: error.detail,
    })?;
    if output.status.success() {
        return Ok(());
    }
    Err(Refusal::NotARepository {
        source: source.to_path_buf(),
        detail: git_error(source, &REPOSITORY, &output).detail,
    })
}

/// The checkout question: `symbolic-ref --quiet HEAD` prints the ref `HEAD` points at on exit 0, and nothing at
/// all on exit 1 — which is a detached `HEAD` and not a failure.
///
/// The answer has to be [`MASTER_REF`] and not merely "there is a `master`": [`clean`] and [`describe`] both
/// answer about `HEAD`, so any other `HEAD` would pair this candidate's commit with another tree's tag and
/// cleanliness.
fn checked_out(source: &Path) -> Result<(), Refusal> {
    let output = git_output(source, &HEAD_REF).map_err(|error| unreadable(source, error.detail))?;
    match output.status.code() {
        Some(0) => {
            let reference = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if reference == MASTER_REF {
                return Ok(());
            }
            Err(Refusal::NotOnMaster {
                source: source.to_path_buf(),
                head: Some(reference),
            })
        }
        Some(1) => Err(Refusal::NotOnMaster {
            source: source.to_path_buf(),
            head: None,
        }),
        _ => Err(unreadable(
            source,
            git_error(source, &HEAD_REF, &output).detail,
        )),
    }
}

/// The commit: `rev-parse --verify --quiet` prints it on exit 0 and nothing on exit 1, which is "no such ref" —
/// and after [`checked_out`] that is an unborn `master` rather than a missing branch, because the checkout's
/// `HEAD` already points at the name.
///
/// Any other exit, or a git that could not be started, is git failing to answer at all — the checkout cannot be
/// read, which is [`Refusal::NotARepository`]'s sentence with git's own words in the detail.
fn master(source: &Path) -> Result<String, Refusal> {
    let output = git_output(source, &MASTER).map_err(|error| unreadable(source, error.detail))?;
    match output.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&output.stdout).trim().to_string()),
        Some(1) => Err(Refusal::NoMaster {
            source: source.to_path_buf(),
            detail: "git has no such ref".to_string(),
        }),
        _ => Err(unreadable(
            source,
            git_error(source, &MASTER, &output).detail,
        )),
    }
}

/// The clean check: `status --porcelain -z` prints one NUL-terminated record per piece of work, and nothing at all
/// on a clean tree.
///
/// `-z` because an entry exists to tell the operator which file to deal with, and a path with a non-ASCII byte
/// would otherwise come back quoted and octal-escaped; `--no-renames` so a rename is the two records it is (`A`
/// the new path, `D` the old) and not a record followed by a bare path with no status columns in front of it.
fn clean(source: &Path) -> Result<(), Refusal> {
    let output = git_output(source, &STATUS).map_err(|error| unreadable(source, error.detail))?;
    if !output.status.success() {
        return Err(unreadable(
            source,
            git_error(source, &STATUS, &output).detail,
        ));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<String> = listing
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect();
    if entries.is_empty() {
        return Ok(());
    }
    Err(Refusal::Dirty {
        source: source.to_path_buf(),
        entries,
    })
}

/// The tag: `git describe --tags --always`, which answers even a repository with no tags (the abbreviated commit).
fn describe(source: &Path) -> Result<String, Refusal> {
    let output = git_output(source, &DESCRIBE).map_err(|error| unreadable(source, error.detail))?;
    if !output.status.success() {
        return Err(unreadable(
            source,
            git_error(source, &DESCRIBE, &output).detail,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Git could not read the checkout at all: the repository refusal's sentence, with git's own last words.
fn unreadable(source: &Path, detail: String) -> Refusal {
    Refusal::NotARepository {
        source: source.to_path_buf(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirty(entries: &[&str]) -> Refusal {
        Refusal::Dirty {
            source: PathBuf::from("/src/holosmith"),
            entries: entries.iter().map(|entry| entry.to_string()).collect(),
        }
    }

    #[test]
    fn an_unset_source_is_a_machine_with_no_candidate() {
        assert_eq!(resolve(&Config::default()), Err(Refusal::NoCandidate));
        let sentence = Refusal::NoCandidate.to_string();
        assert!(sentence.contains("no candidate"), "{sentence}");
        assert!(sentence.contains("`[update] source`"), "{sentence}");
        assert!(sentence.contains("holosmith config edit"), "{sentence}");
    }

    #[test]
    fn the_dirty_sentence_lists_the_first_few_entries_and_counts_the_rest() {
        // Five entries: the sentence shows three and says how many it did not, so an operator recognises the work
        // without the refusal becoming a listing.
        assert_eq!(
            shown(&[
                "M src/a.rs".to_string(),
                "M src/b.rs".to_string(),
                "M src/c.rs".to_string(),
                "M src/d.rs".to_string(),
                "M src/e.rs".to_string(),
            ]),
            "M src/a.rs, M src/b.rs, M src/c.rs, and 2 more"
        );
        assert_eq!(
            shown(&["?? notes.txt".to_string()]),
            "?? notes.txt",
            "a tree with fewer entries than the constant does not count anything"
        );
        let sentence = dirty(&["M src/a.rs", "M src/b.rs", "M src/c.rs", "M src/d.rs"]).to_string();
        assert!(
            sentence.contains("M src/a.rs, M src/b.rs, M src/c.rs"),
            "{sentence}"
        );
        assert!(sentence.contains("and 1 more"), "{sentence}");
        assert!(
            sentence.contains("git -C /src/holosmith stash --include-untracked"),
            "{sentence}"
        );
        assert!(
            !sentence.contains("M src/d.rs"),
            "the fourth entry is counted rather than printed: {sentence}"
        );
    }

    #[test]
    fn a_stale_request_names_both_commits() {
        let candidate = Candidate {
            source: PathBuf::from("/src/holosmith"),
            commit: "2222222222222222222222222222222222222222".to_string(),
            tag: "v1.0.0".to_string(),
        };
        assert_eq!(candidate.stale(&candidate.commit), None);
        let stale = candidate
            .stale("1111111111111111111111111111111111111111")
            .expect("a different commit is stale");
        assert_eq!(stale.requested, "1111111111111111111111111111111111111111");
        assert_eq!(stale.current, candidate.commit);
        let sentence = stale.to_string();
        assert!(sentence.contains(&stale.requested), "{sentence}");
        assert!(sentence.contains(&stale.current), "{sentence}");
        assert!(sentence.contains("promote again"), "{sentence}");
    }

    #[test]
    fn each_refusal_names_the_one_command_that_repairs_it() {
        let repository = Refusal::NotARepository {
            source: PathBuf::from("/src/holosmith"),
            detail: "fatal: not a git repository".to_string(),
        };
        assert!(
            repository
                .to_string()
                .contains("git clone <url> /src/holosmith-src"),
            "{repository}"
        );
        let master = Refusal::NoMaster {
            source: PathBuf::from("/src/holosmith"),
            detail: "git has no such ref".to_string(),
        };
        assert!(
            master.to_string().contains("holosmith config edit"),
            "{master}"
        );
    }

    #[test]
    fn the_head_refusal_names_the_state_it_found_and_the_one_command() {
        let branch = Refusal::NotOnMaster {
            source: PathBuf::from("/src/holosmith"),
            head: Some("refs/heads/feature/x".to_string()),
        };
        let sentence = branch.to_string();
        assert!(sentence.contains("refs/heads/feature/x"), "{sentence}");
        assert!(
            sentence.contains("git -C /src/holosmith checkout master"),
            "{sentence}"
        );
        let detached = Refusal::NotOnMaster {
            source: PathBuf::from("/src/holosmith"),
            head: None,
        };
        let sentence = detached.to_string();
        assert!(sentence.contains("`HEAD` is detached"), "{sentence}");
        assert!(sentence.contains("checkout master"), "{sentence}");
    }
}
