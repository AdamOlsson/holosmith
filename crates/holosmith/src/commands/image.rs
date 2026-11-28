//! `holosmith image build` and `holosmith image run`: the two halves of one image, on their own.
//!
//! A project's image is a content address — `hs-<project>:<digest>` over the recipe's bytes and the base image's tag —
//! so "is it built?" is one existence check and the answer is free when it is ([`crate::imagebuild`]). `build` adds
//! *when*: a fresh machine's first session would otherwise pay a six-minute build before it answered anything, and an
//! operator who has just written a recipe wants to know it builds before a session depends on it.
//!
//! `run` adds *where else it runs*: a project that wants to check in its own CI that its image works would otherwise
//! have to reproduce a session's container by hand, and the one flag that gets copied wrong is `--user` — root passes,
//! the image's own user passes, and the invoking uid, the one a session actually uses, may have no passwd entry inside
//! the image. Its shape comes from [`crate::container::SessionShape`], the same value a session's `docker run` takes
//! its lifecycle, hardening, limits and identity from, so the two cannot drift.
//!
//! stdout is the answer and stderr is progress (ADR-0092), in both verbs. `build`'s answer is `built hs-foo:…` or why
//! nothing was built; `run`'s answer is the command's own stdout, so the `docker run` line it used goes to stderr
//! beside the build's progress. A failed build exits 1 with the sentence on stderr — the tail of what the daemon said,
//! and the path of the whole log — except a build whose pinned fetch did not answer, which exits 4: that one is a host
//! that could not reach musl.cc rather than a recipe that will not build, and the code is what `server deploy`'s image
//! step and `scripts/verify.sh`'s `UNREACHABLE` already answer (ADR-0111). Every other refusal is exit 2 and one
//! sentence — the code every verb uses for "this cannot run here" — and `run` exits with its command's own status.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli::ImageAction;
use crate::config::{Config, LoadError};
use crate::container::{ContainerSettings, ImageRun, SessionShape};
use crate::imagebuild::{self, Currency};
use crate::images::BuildError;
use crate::progress::Progress;
use crate::worktree;

pub fn run(action: ImageAction, named: Option<PathBuf>) -> ExitCode {
    let config = match Config::load(named) {
        Ok(loaded) => loaded.config,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: image: {error}");
            return ExitCode::FAILURE;
        }
    };
    let settings = ContainerSettings::of(&config);
    match action {
        ImageAction::Build {
            project,
            base,
            force,
        } => build(&settings, project.as_deref(), base, force),
        ImageAction::Run {
            project,
            work,
            command,
        } => run_in_image(&config, &settings, &project, work, command),
    }
}

/// One `image build`: the base, or a project's own image, as `force` asks.
fn build(settings: &ContainerSettings, project: Option<&str>, base: bool, force: bool) -> ExitCode {
    let currency = if force {
        Currency::Forced
    } else {
        Currency::Missing
    };
    let mut progress = Line { verb: "build" };
    let docker = settings.docker();
    let answer = if base {
        imagebuild::ensure_base(&docker, settings, currency, &mut progress)
    } else {
        let Some(project) = project else {
            // clap refuses this before the config is loaded; the arm is here because the enum allows it and a
            // `None` that panicked would be a panic in the one place a sentence belongs.
            return failure(
                "build",
                "no project was named: `holosmith image build <project>`, or `--base`",
            );
        };
        let root = match project_root(settings, project) {
            Ok(root) => root,
            Err(sentence) => return failure("build", &sentence),
        };
        imagebuild::ensure(&docker, settings, project, &root, currency, &mut progress)
    };
    match answer {
        Ok(sentence) => {
            println!("{sentence}");
            ExitCode::SUCCESS
        }
        Err(error) => build_failed("build", &error),
    }
}

/// One `image run`: the project's image, one command in a session's shape, and the command's own exit status.
///
/// The refusals come in the order the questions get cheaper and more local, so a machine that cannot run anything
/// still answers the ones an author can fix: what to run, which project, its recipe, the work directory, and only
/// then the daemon. Nothing here writes a row, creates a network or touches an event log — it is one `docker run`,
/// and `imagebuild::ensure` is the same build under the same lock and log a create does rather than a second one.
fn run_in_image(
    config: &Config,
    settings: &ContainerSettings,
    project: &str,
    work: Option<PathBuf>,
    command: Vec<String>,
) -> ExitCode {
    if command.is_empty() {
        return refused(
            "run",
            "nothing was named to run: write the command after `--`, as in \
             `holosmith image run <project> -- npm ci`",
        );
    }
    // The sentences are `image build`'s, because they are the same facts about the same name: a name no create would
    // take, and a project that is not there.
    let root = match project_root(settings, project) {
        Ok(root) => root,
        Err(sentence) => return refused("run", &sentence),
    };
    // The create's own `Unusable` sentence (ADR-0123): `ensure` would read the recipe again and answer it, but a
    // recipe that cannot become an image is worth refusing before the daemon question rather than after it.
    let resolved = match crate::projectimage::resolve(&root, project, &settings.image) {
        Ok(resolved) => resolved,
        Err(problem) => return refused("run", &problem.sentence()),
    };
    let work = match work {
        Some(path) => match std::fs::canonicalize(&path) {
            Ok(seen) if seen.is_dir() => seen,
            _ => {
                return refused(
                    "run",
                    &format!(
                        "the work directory {} is not a directory this machine has: `docker run -v` creates a \
                         missing source as an empty root-owned directory instead of mounting it, so name one that \
                         exists and that the daemon can see",
                        path.display()
                    ),
                );
            }
        },
        // The project's own checkout, which is what a session's worktree is a branch of. It is the mount source, so
        // it has to be a path the daemon can see for ADR-0035's reason: a projects root on a tmpfs the daemon does
        // not share would give the command an empty `/work`.
        None => root.clone(),
    };
    let daemon = crate::preflight::daemon(config);
    if daemon.is_failure() {
        return refused("run", &daemon.sentence);
    }
    let docker = settings.docker();
    let mut progress = Line { verb: "run" };
    // The build's answer — `built hs-foo:…`, or why nothing was built — is not repeated: the progress sink already
    // said it, and this verb's stdout belongs to the command.
    if let Err(error) = imagebuild::ensure(
        &docker,
        settings,
        project,
        &root,
        Currency::Missing,
        &mut progress,
    ) {
        return build_failed("run", &error);
    }
    let run = ImageRun {
        image: resolved.image,
        work,
        command,
        shape: SessionShape::of(&settings.data_root, settings.limits),
    };
    let arguments = run.run_args();
    // The line is progress and not the answer, which is why it is stderr: a pipeline that captures the command's
    // stdout gets the command's stdout and nothing else.
    eprintln!("holosmith: image run: {}", docker.line(&arguments));
    match docker.command_line(&arguments).status() {
        Ok(status) => exit_of(status),
        Err(error) => refused(
            "run",
            &format!("the docker command could not be run ({error})"),
        ),
    }
}

/// The project's checkout, or the sentence saying why `project` is not one of this machine's projects.
///
/// **The name is checked before it is joined, because what this answers becomes a directory** — and one caller mounts
/// it **writable** at [`crate::container::WORKDIR`]. `worktree::repository_of`'s own doc says it is the naming rule
/// and not the guard, and `projectimage::resolve` asks the name rule only when a recipe exists — so `..`, which no
/// create would accept, would resolve `<projects_root>/..` (the machine's whole root) and find no recipe to refuse it.
/// The rule is the create's ([`crate::projects::name_is_valid`]) asked here, once for both verbs.
///
/// A valid name with no directory under the projects root is the second sentence: a name the operator mistyped, and it
/// names the directory that was looked in, because `resolve` would answer "no recipe" for a directory that is not
/// there — a different fact from the one worth telling them.
fn project_root(settings: &ContainerSettings, project: &str) -> Result<PathBuf, String> {
    if !crate::projects::name_is_valid(project) {
        return Err(format!(
            "`{project}` is not a project name any create would take: a name is one path segment of letters, \
             digits, `.`, `_` and `-`, at most {} characters, and not `.` or `..`",
            crate::projects::MAX_NAME_CHARS
        ));
    }
    let root = worktree::repository_of(&settings.projects_root, project);
    if !root.is_dir() {
        return Err(format!(
            "the project `{project}` is not under {}: add it with `holosmith project add`, or name one that is",
            settings.projects_root.display()
        ));
    }
    Ok(root)
}

/// A command's own exit status as this process's: its code, or 128 plus the signal that killed it, which is what a
/// shell reports for the same ending.
fn exit_of(status: std::process::ExitStatus) -> ExitCode {
    use std::os::unix::process::ExitStatusExt as _;
    let code = status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal));
    ExitCode::from(code.and_then(|code| u8::try_from(code).ok()).unwrap_or(1))
}

/// One build failure, for either verb: exit 1, or 4 when a pinned fetch did not answer (ADR-0111).
///
/// A build that failed is neither verb's refusal — nothing about the invocation was wrong — so it keeps `build`'s
/// two codes rather than becoming 2.
fn build_failed(verb: &str, error: &BuildError) -> ExitCode {
    if error.fetch_did_not_answer {
        // The tree was never judged: a pinned fetch that did not answer is `server deploy`'s step 3 answering 4, and a
        // caller that read it as 1 would record a refusal nobody made (ADR-0111).
        eprintln!("holosmith: image {verb}: {}", error.sentence);
        return ExitCode::from(crate::deploy::FETCH_UNANSWERED);
    }
    failure(verb, &error.sentence)
}

/// One sentence on stderr and a failed exit, which is every failure a build leaves.
fn failure(verb: &str, sentence: &str) -> ExitCode {
    eprintln!("holosmith: image {verb}: {sentence}");
    ExitCode::FAILURE
}

/// One sentence on stderr and exit 2: a call this machine cannot carry out, as every other verb answers one.
fn refused(verb: &str, sentence: &str) -> ExitCode {
    eprintln!("holosmith: image {verb}: {sentence}");
    ExitCode::from(2)
}

/// These verbs' progress sink: one line per fact on stderr, flushed.
///
/// The label is dropped for [`crate::progress::Announce`]'s reason read backwards: that sink prefixes a deploy's step
/// number because its report has one, and these verbs' reports are the line on stdout. The verb word is kept, so a
/// line says which of the two runs it belongs to.
struct Line {
    verb: &'static str,
}

impl Progress for Line {
    fn note(&mut self, _label: &str, phrase: &str) {
        use std::io::Write as _;
        let mut stderr = std::io::stderr();
        let _ = writeln!(stderr, "holosmith: image {}: {phrase}", self.verb);
        let _ = stderr.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt as _;

    /// Both halves of the verb's answer, including the one no case can measure: the gate runs a command that exits 7,
    /// and a command a signal ended is a status no `sh -c` in the gate produces.
    #[test]
    fn a_status_is_its_code_or_128_plus_the_signal_that_ended_it() {
        // `from_raw` takes a wait status: the code is the high byte, the signal the low seven bits, so `9` is SIGKILL
        // and `7 << 8` is exit 7 — a shell reports that one as `killed` and 137.
        assert_eq!(
            exit_of(std::process::ExitStatus::from_raw(9)),
            ExitCode::from(137)
        );
        assert_eq!(
            exit_of(std::process::ExitStatus::from_raw(7 << 8)),
            ExitCode::from(7)
        );
    }
}
