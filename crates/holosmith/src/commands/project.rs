//! `holosmith project add|list`: how a repository becomes a project this machine can run sessions against.
//!
//! `add` clones a URL into `<projects_root>/<name>` and prints `<name>` — the exact string the new-session
//! form takes — because the name a project is addressed by is derived here rather than asked for (ADR-0096).
//! `list` prints the names a create would accept, which is the same question read back.
//!
//! The clone is `git clone` on the host, as one child process, and what it produced is put through
//! [`crate::worktree::refuse`] before the command answers 0: a clone a create would refuse is not a project
//! that was added, so it is removed and the refusal is printed in the API's own code and sentence. The
//! removal is what keeps the command retryable — `add` refuses a destination that already exists, so a
//! refused clone left in place would be a name nothing can ever be added under.
//!
//! stdout is the verdict and stderr is progress (ADR-0092). The name is the only byte on stdout, because
//! `name=$(holosmith project add <url>)` is the use the printed name exists for; `cloning …` and every
//! failure's one sentence are on stderr.

use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use crate::cli::ProjectAction;
use crate::config::{Config, LoadError};
use crate::http::refusal_error;
use crate::projects;
use crate::worktree::{self, Refusal};

pub fn run(action: ProjectAction, named: Option<std::path::PathBuf>) -> ExitCode {
    let config = match Config::load(named) {
        Ok(loaded) => loaded.config,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: project: {error}");
            return ExitCode::FAILURE;
        }
    };
    match action {
        ProjectAction::Add { url } => add(&config.projects_root, &url),
        ProjectAction::List => list(&config.projects_root),
    }
}

/// Clone `url` into `<projects_root>/<derived name>` and print the name.
fn add(projects_root: &Path, url: &str) -> ExitCode {
    let name = match projects::name_from_url(url) {
        Ok(name) => name,
        Err(sentence) => return failure("add", &sentence),
    };
    let destination = projects_root.join(&name);
    // `symlink_metadata`, not `exists`: a broken symlink is still something at that path, and a clone over it
    // would fail inside git with a sentence about git rather than about the name being taken.
    if std::fs::symlink_metadata(&destination).is_ok() {
        return failure(
            "add",
            &format!(
                "{} already exists, so {url} was not cloned: remove it, or add a repository whose name is \
                 not `{name}`",
                destination.display()
            ),
        );
    }
    // git creates the last segment of the destination and not the root above it, and the root is the one
    // directory every other command assumes is there.
    if let Err(error) = std::fs::create_dir_all(projects_root) {
        return failure(
            "add",
            &format!("cannot make {}: {error}", projects_root.display()),
        );
    }
    eprintln!(
        "holosmith: project add: cloning {url} into {}",
        destination.display()
    );
    // Captured rather than inherited: git's own banner and progress meter would otherwise be lines on the
    // stream a failure has one line of, and one of them is stdout — where the name has to be alone. The
    // failure keeps git's last word, which is the sentence that says why.
    let output = match Command::new("git")
        .arg("clone")
        .arg("--")
        .arg(url)
        .arg(&destination)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            // Nothing was cloned, and git may still have made the directory before failing: the path is ours.
            let _ = discard(&destination);
            return failure("add", &format!("git could not be run: {error}"));
        }
    };
    if !output.status.success() {
        // A clone git gave up on leaves nothing or a half-written directory, and either way the path is ours:
        // the check above refused one that was already there.
        let _ = discard(&destination);
        return failure(
            "add",
            &format!(
                "git clone {url} failed: {}",
                last_line(&String::from_utf8_lossy(&output.stderr))
            ),
        );
    }
    if let Err(refusal) = worktree::refuse(&destination) {
        let mut line = refusal_line(refusal);
        match discard(&destination) {
            Ok(()) => line.push_str(&format!(
                " The clone at {} was removed, so a repository that fits the name `{name}` can be added.",
                destination.display()
            )),
            Err(error) => line.push_str(&format!(
                " {} could not be removed, so it is in the way of a retry: {error}",
                destination.display()
            )),
        }
        return failure("add", &line);
    }
    println!("{name}");
    ExitCode::SUCCESS
}

/// Print the name of every directory the create's repository refusals accept, and nothing else.
fn list(projects_root: &Path) -> ExitCode {
    let entries = match std::fs::read_dir(projects_root) {
        Ok(entries) => entries,
        // A machine that has added no project has no projects root yet, which is an empty list and not a
        // failure: the first `project add` makes the directory.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ExitCode::SUCCESS,
        Err(error) => {
            return failure(
                "list",
                &format!("cannot read {}: {error}", projects_root.display()),
            );
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // The filter is the create's own: a directory it would refuse with a code is not a name the form can
        // use, so printing it would be a name that fails one step later.
        if !path.is_dir() || worktree::refuse(&path).is_err() {
            continue;
        }
        // A directory name that is not UTF-8 cannot be typed into the form either, so it is skipped rather
        // than printed lossily.
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            names.push(name.to_string());
        }
    }
    names.sort();
    for name in names {
        println!("{name}");
    }
    ExitCode::SUCCESS
}

/// One failure's one sentence, named for the subcommand that failed, on stderr; stdout stays the verdict.
fn failure(action: &str, sentence: &str) -> ExitCode {
    eprintln!("holosmith: project {action}: {sentence}");
    ExitCode::FAILURE
}

/// The create's refusal as one line: the API's code and the API's sentence.
///
/// [`refusal_error`] is the route's own mapping, so a project this command refuses and a project a create
/// refuses are refused in the same words. `Unreadable` is the arm with no code — git could not be asked about
/// the clone at all, which is not one of the three shapes a repository is refused for — and there the
/// operator gets git's own words, which is where `Refusal`'s doc sends them.
fn refusal_line(refusal: Refusal) -> String {
    match refusal {
        Refusal::Unreadable(error) => error.to_string(),
        shape => {
            let answer = refusal_error(shape);
            format!("{}: {}", answer.body.code, answer.body.message)
        }
    }
}

/// Remove what this run cloned, because nothing that was here before it can be under this path.
fn discard(destination: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The last non-empty line of a child's stream, which is where a program puts the reason it stopped.
fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::error::{
        ApiError, GIT_NOT_DIRECTORY, SUBMODULES_PRESENT, WORKTREE_CONFIG_ENABLED,
    };

    /// One code and one sentence per refused shape, and they are the route's own.
    ///
    /// This is where two of the three are measured rather than in `tests/project_cli.rs`: `git clone` cannot
    /// produce either shape. A fresh clone always has a `.git` **directory**, so `git_not_directory` is
    /// unreachable through this command; and `extensions.worktreeConfig` is a repository-local setting git
    /// does not copy into a clone, so `worktree_config_enabled` is unreachable through it too. A test that
    /// faked one of them would be asserting the mapping against a clone the command never sees, so the
    /// mapping is asserted here, against the constructors the create answers with.
    #[test]
    fn a_refused_clone_is_refused_in_the_api_s_own_code_and_sentence() {
        for (refusal, code, answer) in [
            (
                Refusal::NotADirectory,
                GIT_NOT_DIRECTORY,
                ApiError::git_not_directory(),
            ),
            (
                Refusal::SubmodulesPresent,
                SUBMODULES_PRESENT,
                ApiError::submodules_present(),
            ),
            (
                Refusal::WorktreeConfigEnabled,
                WORKTREE_CONFIG_ENABLED,
                ApiError::worktree_config_enabled(),
            ),
        ] {
            assert_eq!(
                refusal_line(refusal.clone()),
                format!("{code}: {}", answer.body.message)
            );
            // And the route maps that same `Refusal` to that same pair, which is what makes the two one
            // decision rather than two copies of one.
            let route = refusal_error(refusal);
            assert_eq!(route.body.code, answer.body.code);
            assert_eq!(route.body.message, answer.body.message);
        }
    }

    /// A clone git could not read is not one of the three shapes, and its sentence is git's.
    #[test]
    fn a_repository_git_cannot_read_answers_gits_own_words() {
        let refusal = Refusal::Unreadable(worktree::GitError {
            command: "git config".to_string(),
            detail: "not a git repository".to_string(),
        });
        assert_eq!(refusal_line(refusal), "git config: not a git repository");
    }

    #[test]
    fn the_last_line_is_the_last_one_with_something_in_it() {
        assert_eq!(last_line("a\nb\n\n"), "b");
        assert_eq!(last_line(""), "no output");
    }
}
