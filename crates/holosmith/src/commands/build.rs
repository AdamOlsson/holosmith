//! `holosmith build`: compile a release artifact in the pinned builder container.
//!
//! A **host** verb, and a thin one: it reads the config the way every command does (so `--config` means one thing
//! and `docker_host` is honoured), resolves what it was asked for, and hands [`crate::builder`] three questions —
//! is the daemon there, is the image this recipe's, did the container build it. What this module owns is the order
//! of those questions and the lines an operator reads.
//!
//! The order matters. The daemon is asked **first**: a build that got as far as the mount and then found no daemon
//! would leave an output directory that reads as a build's. The image is ensured second, because a build with no
//! image is a six-minute wait nobody announced. The container runs third. And the artifact is checked by this side
//! before it is placed — `e_machine` for the triple that was asked for, `ET_EXEC`, no interpreter — because a
//! container that exited 0 has told us it built *something*.
//!
//! Every line this prints is `build: ` prefixed, like `deploy:` and `doctor:`, and the last one names the artifact
//! and its digest. The exit code is 0 when an artifact is in place (whether it was just built or already there) and
//! 1 for every refusal, including a failed build, whose container's last [`crate::builder::TAIL_LINES`] lines are
//! printed above the sentence.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::builder;
use crate::config::{Config, LoadError};
use crate::docker::Docker;
use crate::images;

/// What the command line asked for: the config every command reads, the artifact to build, and where it goes.
#[derive(Debug)]
pub struct BuildArgs {
    pub config: Option<PathBuf>,
    /// The triple to build. Absent means this binary's own architecture (see [`builder::default_target`]).
    pub target: Option<String>,
    /// The directory the artifact and its digest land in. Absent means item 10's `releases/<tag>/`.
    pub out: Option<PathBuf>,
}

pub fn run(args: BuildArgs) -> ExitCode {
    match execute(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(said) => {
            eprintln!("holosmith: build: {said}");
            ExitCode::FAILURE
        }
    }
}

fn execute(args: BuildArgs) -> Result<(), String> {
    let loaded = match Config::load(args.config.clone()) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            return Err(format!("{problems}").trim_end().to_string());
        }
        Err(error) => return Err(format!("the config could not be read: {error}")),
    };
    let docker = Docker::new(loaded.config.docker_host.clone());

    // The source is the current directory: an agent works in a worktree and runs this there, and a developer's
    // checkout is where `git describe` finds the tag. Nothing is copied out of it by this side.
    let source = std::env::current_dir()
        .map_err(|error| format!("the current directory could not be read ({error})"))?;
    if let Some(problem) = builder::source_problem(&source) {
        return Err(problem);
    }
    let target = match args.target {
        Some(target) => target,
        None => builder::default_target()?.to_string(),
    };
    let tag = builder::tag(&source);
    let out = match args.out {
        Some(out) => out,
        None => PathBuf::from(builder::RELEASES).join(&tag),
    };
    let out = builder::absolute(&out)?;
    std::fs::create_dir_all(&out).map_err(|error| {
        format!(
            "the output directory {} could not be created ({error})",
            out.display()
        )
    })?;

    println!("build: source: {} ({tag})", source.display());
    println!("build: target: {target}");

    if let Some(problem) = builder::daemon_problem(&docker) {
        return Err(problem);
    }
    println!("build: builder: {}", builder::ensure_image(&docker)?);

    let owner = builder::owner(&out)?;
    let image = builder::image();
    builder::run(&docker, &image, &source, &out, &tag, &target, owner)?;

    // The bytes the container wrote, read once: the ELF checks, the digest and the placement are all about them.
    let pending = out.join(builder::PENDING);
    let bytes = std::fs::read(&pending).map_err(|error| {
        format!(
            "the container's build finished but wrote no artifact to {} ({error}): the build's last step copies \
             the binary there, so a missing file means the entry script changed without this command knowing",
            pending.display()
        )
    })?;
    if let Some(problem) = builder::artifact_problem(&pending, &bytes, &target) {
        let _ = std::fs::remove_file(&pending);
        return Err(problem);
    }
    let digest = images::hex(&images::sha256(&bytes));
    let artifact = out.join(builder::BINARY);
    match builder::place(&out, &digest)? {
        builder::Placement::Fresh => {
            println!(
                "build: artifact: {} ({} bytes)",
                artifact.display(),
                bytes.len()
            );
            println!(
                "build: sha256: {digest}  {}",
                out.join(builder::DIGEST).display()
            );
        }
        builder::Placement::Current => {
            println!(
                "build: artifact: {} is already this build (sha256 {digest})",
                artifact.display()
            );
        }
        builder::Placement::Different { existing, built } => {
            return Err(format!(
                "{} already holds a different binary for {tag} (sha256 {existing}, and this build is {built}): \
                 nothing was replaced, because one release directory holding two binaries is the ambiguity a \
                 promotion cannot resolve — move that directory away, or pass `--out`",
                artifact.display()
            ));
        }
    }
    Ok(())
}
