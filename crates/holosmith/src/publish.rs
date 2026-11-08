//! The publish: a session's branch pushed to a remote this machine is allowed to push to (HS-057, spec item
//! 12's publish paragraph).
//!
//! Item 12's paragraph, in full, is what this module answers:
//!
//! ```text
//! `POST /api/sessions/:id/publish` with `{remote, branch?, confirm:true}`. The server pushes the session
//! branch `agent/<id>` using the host's own git credentials. Rules: only remotes named in `publish.remotes`
//! config (empty by default, so the endpoint is disabled); never force; refuse protected names (main, master,
//! release/*); `touches_ci` is true when the diff changes `.github/`, other CI files or hook directories, and
//! the UI shows a warning before confirm. Pushing the merged target branch is not supported from the app; push
//! main from the host. Containers still never hold a git credential.
//! ```
//!
//! **This module reaches out of the machine.** Everything before it either read the project repository
//! ([`crate::changes`]) or wrote to it ([`crate::merge`]); a publish writes to somebody else's repository, over
//! whatever transport `remote` names, as **the server's own user** — the host's git, with the host's
//! environment and therefore the host's credential store. Nothing here touches the session's container, its
//! network or its key proxy: a container's network is `--internal` and its four mounts carry no credential
//! (ADR-0035), which is the fact the story's last acceptance bullet asserts. The push is not routed through
//! the egress proxy either — that proxy is a session's route out, and this is the host's.
//!
//! Three decisions shape the code:
//!
//! - **`branch` names the remote's ref, not ours.** The spec's sentence is "pushes the session branch
//!   `agent/<id>`", so the ref *pushed from* is fixed and `branch` can only name the destination: the refspec
//!   is `refs/heads/agent/<id>:refs/heads/<branch>`, and an absent `branch` means the destination is
//!   `agent/<id>` too. (`branch` is *not* a second local branch to push — that reading would make this a
//!   general push endpoint, which "the server pushes the session branch" rules out.)
//! - **The protected names are a constant, and the merge target is a second fact.** [`PROTECTED`] holds
//!   `main`, `master` and the `release/` prefix: a machine may merge into `trunk` and must still refuse a
//!   destination called `main`. The configured `[merge] targets` are checked *after* it, with their own
//!   refusal, because "a name this build never writes" and "the branch this machine lands on" are two facts
//!   an operator reads differently.
//! - **Never force, and a rejection is the remote's answer.** The refspec has no `+`, the command has no
//!   `--force`, and a failed push is classified from `--porcelain`'s machine-readable ref lines — never by
//!   reading git's prose, and never by retrying with force. The porcelain format is git's own contract (the
//!   same class as `worktree list --porcelain` and `status --porcelain`, which this build already parses), and
//!   it is where `!` and the `[rejected]` summary live.
//!
//! Two pins make the push a measurement rather than a hope, and both were measured on this machine:
//!
//! - **`-c core.hooksPath=/dev/null`**, the merge's own flag one command further out: a `pre-push` hook is a
//!   program the host would otherwise run on an agent-written branch. A real hook fired without the flag and
//!   never with it.
//! - **`--no-follow-tags`**, because a repository's `push.followTags = true` otherwise pushes annotated tags
//!   reachable from the pushed ref *in addition* (measured both ways). A repository config must not change
//!   what this route does to somebody else's repository — ADR-0062's rule for `diff.external`, applied to a
//!   push.
//!
//! What this module does not do: no `confirm` (the **route's** field, ADR-0065 D4's shape), no config read
//! (the handler passes the two lists), no session row (ADR-0063's "a row is not consulted"), no
//! `touches_ci` (the changes view's flag, and the warning HS-058's), and no fetch: a publish pushes what is
//! committed, and a remote that has moved on is a refusal the operator resolves by fetching.

use std::path::Path;
use std::process::{Command, Output};

use serde::Serialize;

use crate::changes::{self, ChangesError};
use crate::worktree::{BRANCH_PREFIX, GitError, Worktree, git_command, git_error, git_output};

/// The destination names this build never pushes to, whatever `publish.remotes` says (spec item 12).
///
/// An entry is either a **directory prefix** (it ends with `/`) or a **name** (compared whole), which is the
/// same two-arm shape [`crate::changes::touches_ci`] uses for its paths and for the same reason: `release/1.0`
/// must be refused while `release-notes` must not. `main` and `master` are item 12's two, and `release/` is
/// its star: the spec's protected names are the ones a branch is *released* from.
pub const PROTECTED: &[&str] = &["main", "master", "release/"];

/// The `-c` option that makes a project's hooks unreachable (the merge's own, [`crate::merge`]).
///
/// Hooks live in the **common** git directory, so a `pre-push` there is reachable from any command run in the
/// project repository — including this push, which runs *there* and not in the session's worktree.
const HOOKS_PATH: (&str, &str) = ("core.hooksPath", "/dev/null");

/// The variable that makes a missing credential a failure rather than a question nobody can answer.
///
/// A server has no terminal: git's first move when a remote asks for a username is to prompt on one, and a
/// route that parked a runtime worker on `Username for 'https://…':` would hang until the operator killed the
/// process. `GIT_TERMINAL_PROMPT=0` turns that into git's own error, which this route answers as a machine
/// failure. It does **not** remove the host's credentials: credential helpers, `~/.git-credentials` and an
/// `ssh-agent` are all still consulted — only the keyboard is taken away.
const TERMINAL_PROMPT: (&str, &str) = ("GIT_TERMINAL_PROMPT", "0");

/// One publish request: item 12's two fields, with the body's shape already refused by the handler.
///
/// `branch` is `None` for "the destination is the session branch's own name", which is the request item 12's
/// sentence describes. `confirm` is deliberately not here: it is the route's field, and a module that pushed
/// on its own caller's word would be a module whose refusals depend on its caller (ADR-0065 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub remote: String,
    pub branch: Option<String>,
}

/// Whether `remote` is one of the remotes the config lets this machine push to (item 12's "only remotes named
/// in `publish.remotes`").
///
/// One predicate with two callers, and the pair is deliberate, exactly as [`crate::merge::listed`] is: the
/// handler asks it **before** it reads the session (a body naming a remote this machine does not push to is
/// malformed whatever the session's state is), and [`publish`] asks it again as its own precondition.
pub fn listed(remote: &str, remotes: &[String]) -> bool {
    remotes.iter().any(|listed| listed == remote)
}

/// Whether `name` is a protected destination — one of [`PROTECTED`]'s names or under one of its prefixes.
///
/// The two arms are exclusive: an entry ending in `/` is compared against the whole name's prefix, and every
/// other entry against the whole name. Applying both tests to every entry is the mistake `touches_ci`'s first
/// version made, and it is what would let `releaser/x` through a `/`-entry check and `./main` through a name
/// check.
pub fn protected(name: &str) -> bool {
    PROTECTED.iter().any(|entry| {
        if let Some(prefix) = entry.strip_suffix('/') {
            name.len() > prefix.len() + 1
                && name.starts_with(prefix)
                && name.as_bytes()[prefix.len()] == b'/'
        } else {
            name == *entry
        }
    })
}

/// The ref name the push creates on the remote: `branch` when the request named one, the session branch
/// otherwise.
///
/// Derived from the session's **id** rather than from a [`Worktree`], because the handler has to ask two
/// config-dependent questions about it (protected? the target?) before it reads the session row at all.
pub fn destination(id: &str, request: &Request) -> String {
    match &request.branch {
        Some(branch) => branch.clone(),
        None => format!("{BRANCH_PREFIX}{id}"),
    }
}

/// What a publish did, and the whole of the answer: the remote as asked, the destination's short name, and
/// the commit that **was** the branch's tip when the push began.
///
/// The commit is read *before* the push, from the branch itself, and never taken from the push's own output:
/// git makes no contract about that output, and a field named `commit` must be a commit this build measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pushed {
    pub remote: String,
    pub branch: String,
    pub commit: String,
}

/// Why a publish did not happen — one variant per fact an operator can act on.
///
/// The variants carry the names the *log* is allowed to mention: the envelope's messages are fixed sentences
/// (ADR-0008), so a remote name a sentence would have carried goes to the log instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishError {
    /// No branch to publish, or git could not be asked — [`ChangesError`]'s own two families.
    Changes(ChangesError),
    /// `remote` is not one of the configured `[publish] remotes`: the parameter is not one this machine
    /// accepts.
    Unlisted {
        remote: String,
        remotes: Vec<String>,
    },
    /// `remote` is configured, and the project's repository has no such remote: still the parameter's fault,
    /// since `publish.remotes` naming a remote a project does not have is the ordinary case (a machine's
    /// reminder that the name is per-machine and the URL is per-repository).
    NoRemote { remote: String },
    /// The destination is a protected name ([`PROTECTED`]): a branch this build never writes, whatever the
    /// request says.
    Protected { branch: String },
    /// The destination is one of this machine's `[merge] targets`: pushing it is not supported from the app
    /// ("push main from the host").
    Target {
        branch: String,
        targets: Vec<String>,
    },
    /// The remote refused the push. `reason` is the machine-readable summary's own word (`fetch first`,
    /// `non-fast-forward`, `stale info`, …) and is only ever logged.
    Rejected {
        remote: String,
        branch: String,
        reason: String,
    },
}

impl From<ChangesError> for PublishError {
    fn from(error: ChangesError) -> Self {
        Self::Changes(error)
    }
}

impl From<GitError> for PublishError {
    fn from(error: GitError) -> Self {
        Self::Changes(ChangesError::Git(error))
    }
}

impl PublishError {
    /// The sentence an operator reads in the log, with the names the envelope is not allowed to carry.
    pub fn sentence(&self) -> String {
        match self {
            Self::Changes(error) => error.sentence(),
            Self::Unlisted { remote, remotes } => format!(
                "the remote {remote} is not one of the configured publish remotes [{}]",
                remotes.join(", ")
            ),
            Self::NoRemote { remote } => {
                format!("this project's repository has no remote named {remote}")
            }
            Self::Protected { branch } => {
                format!("{branch} is a protected branch name, so it is never pushed")
            }
            Self::Target { branch, targets } => format!(
                "{branch} is one of this machine's merge targets [{}], which the app does not push",
                targets.join(", ")
            ),
            Self::Rejected {
                remote,
                branch,
                reason,
            } => format!("the remote {remote} rejected {branch}: {reason}"),
        }
    }
}

/// Push `worktree`'s branch to `request`'s remote, or refuse with the fact that stopped it.
///
/// The order is the request's facts, then the repository's, then the write: `remote` listed → destination
/// protected → destination is the target → the branch's tip ([`changes::basis`], whose four refusals are the
/// session's own state) → the remote exists in this repository → the push. Nothing before the last step
/// writes anything, so every refusal leaves the remote and the branch exactly as they were.
///
/// `remotes` and `targets` are the handler's config values rather than this module's reads, for the reason
/// `merge()` takes `targets`: the module is handed the machine's rules, and the handler is where the config
/// lives.
pub fn publish(
    worktree: &Worktree,
    request: &Request,
    remotes: &[String],
    targets: &[String],
) -> Result<Pushed, PublishError> {
    if !listed(&request.remote, remotes) {
        return Err(PublishError::Unlisted {
            remote: request.remote.clone(),
            remotes: remotes.to_vec(),
        });
    }
    let branch = destination(&worktree.id, request);
    if protected(&branch) {
        return Err(PublishError::Protected { branch });
    }
    if targets.iter().any(|target| target == &branch) {
        return Err(PublishError::Target {
            branch,
            targets: targets.to_vec(),
        });
    }
    // The branch's tip, and the four state refusals that say a session has nothing to publish. `basis` is the
    // changes view's (ADR-0063 D1): one rule for "is there a branch here", imported rather than copied. The
    // tip is read **before** the push, so the answer names the commit that was pushed and not whatever the
    // branch has become while the remote was answering.
    let commit = changes::basis(worktree)?;
    if !remote_exists(&worktree.repository, &request.remote)? {
        return Err(PublishError::NoRemote {
            remote: request.remote.clone(),
        });
    }
    let refspec = format!("refs/heads/{}:refs/heads/{branch}", worktree.branch);
    let args = push_arguments(&request.remote, &refspec);
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = push_output(&worktree.repository, &borrowed)?;
    if !output.status.success() {
        // The remote's answer, read from the machine format. A failure with no `[rejected]` line is not a
        // decision anybody made — an unreachable remote, a credential the host does not have — so it is a
        // machine failure carrying git's own words, never a refusal invented from prose.
        if let Some(reason) = rejected_reason(&output.stdout) {
            return Err(PublishError::Rejected {
                remote: request.remote.clone(),
                branch,
                reason,
            });
        }
        return Err(PublishError::Changes(ChangesError::Git(git_error(
            &worktree.repository,
            &borrowed,
            &output,
        ))));
    }
    Ok(Pushed {
        remote: request.remote.clone(),
        branch,
        commit,
    })
}

/// The `git push` argument list, in one place so the two things this build must never do are testable.
///
/// No `--force`, no `-f`, and a refspec with no leading `+`: a non-fast-forward is the remote's answer and this
/// route's refusal, never a retry that overwrites somebody's commit. `-c core.hooksPath=/dev/null` keeps a
/// `pre-push` hook from running on the host, `--porcelain` is the machine-readable output the rejection is read
/// from, and `--no-follow-tags` stops a repository's `push.followTags` from making this push carry tags.
fn push_arguments(remote: &str, refspec: &str) -> Vec<String> {
    vec![
        "-c".to_string(),
        format!("{}={}", HOOKS_PATH.0, HOOKS_PATH.1),
        "push".to_string(),
        "--porcelain".to_string(),
        "--no-follow-tags".to_string(),
        remote.to_string(),
        refspec.to_string(),
    ]
}

/// The push's `Command`: the host's own `git`, with the host's environment inherited and one variable added.
///
/// Inherited and not scrubbed, because "using the host's own git credentials" means exactly that: whatever
/// `HOME`, `SSH_AUTH_SOCK` and `GIT_CONFIG_*` the server's user has are the push's. The one addition is
/// [`TERMINAL_PROMPT`], which takes away a keyboard a server does not have.
fn push_command(repository: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository)
        .args(args)
        .env(TERMINAL_PROMPT.0, TERMINAL_PROMPT.1);
    command
}

/// Run the push and collect its output; a git that cannot be started is the same sentence the build's other
/// runners give.
fn push_output(repository: &Path, args: &[&str]) -> Result<Output, GitError> {
    push_command(repository, args)
        .output()
        .map_err(|error| GitError {
            command: git_command(repository, args),
            detail: format!("git is not runnable ({error}): install git 2.38 or newer"),
        })
}

/// Whether this project's repository has a remote called `remote`, asked of git rather than of the config.
///
/// `git remote get-url` exits 0 with the URL and **2** when there is no such remote (measured); anything else
/// is a question that could not be asked, which is a failure rather than a `false`. Without this pre-check the
/// same fact would reach the operator as a `500` carrying `fatal: 'origin' does not appear to be a git
/// repository`, which describes the machine's problem as if it were a bug.
fn remote_exists(repository: &Path, remote: &str) -> Result<bool, GitError> {
    let args = ["remote", "get-url", remote];
    let output = git_output(repository, &args)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(2) => Ok(false),
        _ => Err(git_error(repository, &args, &output)),
    }
}

/// The reason a push was rejected, read from `--porcelain`'s ref lines, or `None` when the remote rejected
/// nothing.
///
/// The format is `<flag>\t<from>:<to>\t<summary> (<reason>)`, and a rejected ref is the one whose flag is `!`.
/// The parenthesis is git's own word for the fact — `fetch first`, `non-fast-forward`, `stale info`, `remote
/// rejected` — and it is returned as written for the **log**: no decision in this build depends on its
/// spelling, and a word this build has not seen is still a rejection.
fn rejected_reason(stdout: &[u8]) -> Option<String> {
    for line in String::from_utf8_lossy(stdout).lines() {
        let mut fields = line.split('\t');
        if fields.next() != Some("!") {
            continue;
        }
        let summary = fields.next_back().unwrap_or_default().trim();
        let reason = match (summary.find('('), summary.rfind(')')) {
            (Some(open), Some(close)) if close > open => summary[open + 1..close].trim(),
            _ => summary,
        };
        return Some(if reason.is_empty() {
            "rejected".to_string()
        } else {
            reason.to_string()
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    #[test]
    fn the_push_line_never_forces_and_never_carries_tags_or_hooks() {
        // The two `never`s of item 12's paragraph, on the one vector that decides them. Dropping
        // `--no-follow-tags` pushes a repository's annotated tags, dropping `-c core.hooksPath=/dev/null` runs a
        // `pre-push` hook, and a `+` in the refspec would be a force — measured in `tests/publish.rs` and here.
        let arguments = push_arguments("origin", "refs/heads/agent/x:refs/heads/agent/x");
        let texts = texts(&arguments);
        assert_eq!(texts.first(), Some(&"-c"));
        assert_eq!(texts.get(1), Some(&"core.hooksPath=/dev/null"));
        assert_eq!(texts.get(2), Some(&"push"));
        assert_eq!(texts.get(3), Some(&"--porcelain"));
        assert_eq!(texts.get(4), Some(&"--no-follow-tags"));
        assert_eq!(texts.get(5), Some(&"origin"));
        assert_eq!(texts.get(6), Some(&"refs/heads/agent/x:refs/heads/agent/x"));
        assert_eq!(texts.len(), 7);
        for forbidden in [
            "--force",
            "-f",
            "--force-with-lease",
            "--mirror",
            "--all",
            "--tags",
        ] {
            assert!(
                !texts.contains(&forbidden),
                "{forbidden} is in the push vector"
            );
        }
        assert!(
            !texts.iter().any(|argument| argument.starts_with('+')),
            "a refspec with `+` is a force-push"
        );
    }

    #[test]
    fn the_push_cannot_prompt_for_a_credential_and_injects_none() {
        // A server has no terminal, so the one variable is set. The other half matters as much: the command sets
        // **no** credential of its own, so the push is the host's git with the host's own store.
        let command = push_command(Path::new("/projects/holosmith"), &["push", "origin"]);
        let environment: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            environment,
            vec![("GIT_TERMINAL_PROMPT".to_string(), Some("0".to_string()))],
            "the push sets exactly one variable, and no credential"
        );
    }

    #[test]
    fn the_protected_names_are_two_arms_and_names_that_resemble_them_are_not_protected() {
        // Item 12's three names, and the four shapes that a single-arm predicate gets wrong: a prefix applied to
        // a whole name would protect `mainline`, and a name compared against a prefix entry would miss
        // `release/1.0`.
        for name in [
            "main",
            "master",
            "release/1.0",
            "release/2026-01",
            "release/x/y",
        ] {
            assert!(protected(name), "{name} is protected");
        }
        for name in [
            "mainline",
            "mains",
            "Main",
            "release-notes",
            "releaser/x",
            "release",
            "release/",
            "agent/0123456789abcdef",
            "pr/one",
        ] {
            assert!(!protected(name), "{name} is not protected");
        }
    }

    #[test]
    fn the_destination_is_the_request_s_branch_or_the_session_branch() {
        let named = Request {
            remote: "origin".to_string(),
            branch: Some("pr/one".to_string()),
        };
        assert_eq!(destination("0123456789abcdef", &named), "pr/one");
        let unnamed = Request {
            remote: "origin".to_string(),
            branch: None,
        };
        assert_eq!(
            destination("0123456789abcdef", &unnamed),
            "agent/0123456789abcdef"
        );
    }

    #[test]
    fn a_rejection_is_the_porcelain_flag_and_its_reason_is_kept_verbatim() {
        // Three shapes git prints, measured: a fresh branch, an up-to-date ref and a rejection. Only the `!`
        // line is an answer, and the reason is whatever git called it — this build never branches on the word.
        let rejected = b"To /tmp/remote.git\n!\trefs/heads/agent/x:refs/heads/agent/x\t[rejected] (fetch first)\nDone\n";
        assert_eq!(rejected_reason(rejected).as_deref(), Some("fetch first"));
        let moved = b"To /tmp/remote.git\n!\trefs/heads/agent/x:refs/heads/p\t[rejected] (non-fast-forward)\nDone\n";
        assert_eq!(rejected_reason(moved).as_deref(), Some("non-fast-forward"));
        let created =
            b"To /tmp/remote.git\n*\trefs/heads/agent/x:refs/heads/agent/x\t[new branch]\nDone\n";
        assert_eq!(rejected_reason(created), None);
        let up_to_date =
            b"To /tmp/remote.git\n=\trefs/heads/agent/x:refs/heads/p\t[up to date]\nDone\n";
        assert_eq!(rejected_reason(up_to_date), None);
        // git's prose on stderr is not read at all, and a stdout with nothing in it is not a rejection either.
        assert_eq!(rejected_reason(b""), None);
        assert_eq!(
            rejected_reason(b"fatal: could not read from remote repository\n"),
            None
        );
        // A rejection without a parenthesis is still a rejection: the summary is the reason then.
        assert_eq!(
            rejected_reason(b"!\trefs/heads/a:refs/heads/b\t[remote rejected]\n").as_deref(),
            Some("[remote rejected]")
        );
    }

    #[test]
    fn the_remote_this_machine_pushes_to_is_a_membership_question() {
        let remotes = vec!["origin".to_string(), "fork".to_string()];
        assert!(listed("origin", &remotes));
        assert!(listed("fork", &remotes));
        assert!(!listed("upstream", &remotes));
        // An empty list is the documented default, and it lists nothing — which is what makes the endpoint
        // disabled rather than a route that answers "no such remote" for every name.
        assert!(!listed("origin", &[]));
    }
}
