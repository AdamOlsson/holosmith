//! `holosmith image build`: the build a session's first start would otherwise do inside somebody's turn.
//!
//! A project's image is a content address — `hs-<project>:<digest>` over the recipe's bytes and the base image's tag —
//! so "is it built?" is one existence check and the answer is free when it is ([`crate::imagebuild`]). What this verb
//! adds is *when*: a fresh machine's first session would otherwise pay a six-minute build before it answered anything,
//! and an operator who has just written a recipe wants to know it builds before a session depends on it.
//!
//! stdout is the answer and stderr is progress (ADR-0092): `built hs-foo:…` or why nothing was built is the one line a
//! script reads, and `building hs-foo:…` is a line for a person watching. A failed build exits 1 with the sentence on
//! stderr — the tail of what the daemon said, and the path of the whole log — except a build whose pinned fetch did not
//! answer, which exits 4: that one is a host that could not reach musl.cc rather than a recipe that will not build, and
//! the code is what `server deploy`'s image step and `scripts/verify.sh`'s `UNREACHABLE` already answer (ADR-0111).

use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli::ImageAction;
use crate::config::{Config, LoadError};
use crate::container::ContainerSettings;
use crate::imagebuild::{self, Currency};
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
    }
}

/// One `image build`: the base, or a project's own image, as `force` asks.
fn build(settings: &ContainerSettings, project: Option<&str>, base: bool, force: bool) -> ExitCode {
    let currency = if force {
        Currency::Forced
    } else {
        Currency::Missing
    };
    let mut progress = Line;
    let docker = settings.docker();
    let answer = if base {
        imagebuild::ensure_base(&docker, settings, currency, &mut progress)
    } else {
        let Some(project) = project else {
            // clap refuses this before the config is loaded; the arm is here because the enum allows it and a
            // `None` that panicked would be a panic in the one place a sentence belongs.
            return failure("no project was named: `holosmith image build <project>`, or `--base`");
        };
        let root = worktree::repository_of(&settings.projects_root, project);
        // A project that is not there is a name the operator mistyped, and the sentence says which directory was
        // looked in — `resolve` would answer "no recipe" for a directory that does not exist, which is a different
        // fact.
        if !root.is_dir() {
            return failure(&format!(
                "the project `{project}` is not under {}: add it with `holosmith project add`, or name one that is",
                settings.projects_root.display()
            ));
        }
        imagebuild::ensure(&docker, settings, project, &root, currency, &mut progress)
    };
    match answer {
        Ok(sentence) => {
            println!("{sentence}");
            ExitCode::SUCCESS
        }
        Err(error) if error.fetch_did_not_answer => {
            // The tree was never judged: a pinned fetch that did not answer is `server deploy`'s step 3 answering
            // 4, and a caller that read it as 1 would record a refusal nobody made (ADR-0111).
            eprintln!("holosmith: image build: {}", error.sentence);
            ExitCode::from(crate::deploy::FETCH_UNANSWERED)
        }
        Err(error) => failure(&error.sentence),
    }
}

/// One sentence on stderr and a failed exit, which is every refusal this verb has.
fn failure(sentence: &str) -> ExitCode {
    eprintln!("holosmith: image build: {sentence}");
    ExitCode::FAILURE
}

/// This verb's progress sink: one line per fact on stderr, flushed.
///
/// The label is dropped for [`crate::progress::Announce`]'s reason read backwards: that sink prefixes a deploy's step
/// number because its report has one, and this verb's report is the single line on stdout.
struct Line;

impl Progress for Line {
    fn note(&mut self, _label: &str, phrase: &str) {
        use std::io::Write as _;
        let mut stderr = std::io::stderr();
        let _ = writeln!(stderr, "holosmith: image build: {phrase}");
        let _ = stderr.flush();
    }
}
