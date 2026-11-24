//! `holosmith update`: a pass of the gate, then the promotion (ADR-0110).
//!
//! Every promotion today is `holosmith upgrade <binary> --acknowledge-review`, and a job started from a route needs
//! one command whose outcome a file carries rather than a terminal. A pipeline spelled in a handler would be a second
//! entry point to the one pipeline ADR-0082 insists is one command, so the composition is here, in the binary, and
//! it is four steps:
//!
//! 1. **the candidate** — [`crate::candidate::resolve`]'s value: the `master` of the clean checkout `[update] source`
//!    names, and the tag it describes itself with. Nothing here reads git a second way.
//! 2. **the gate** — the candidate's own `<source>/scripts/verify-in-container.sh`, with
//!    `HOLOSMITH_VERIFY_IMAGE=agent-image:<this binary's version>`, full and **not** `--no-build`. It runs the whole
//!    proof in the image the running release deployed, and inside it the gate's `build` phase produces
//!    `dist/<triple>/holosmith` while `case_server_images` builds the *candidate's* product images from the
//!    candidate's own recipes (ADR-0110 D5). **Exit 0 is the acknowledgement**: this command has no
//!    `--acknowledge-review`, because the gate is the record where an operator's flag would be.
//! 3. **the promotion** — [`crate::promote`] on `<source>/dist/<this machine's triple>/holosmith`, exactly as
//!    `upgrade` calls it: smoke, images, drain, backup, release, `current`, restart, the twenty-second poll and the
//!    switch back.
//! 4. **the report** — one JSON object at `--report`, written for **every** ending: a caller that cannot watch a
//!    terminal reads that file and nothing else.
//!
//! The gate's exit codes keep the meanings `scripts/verify-in-container.sh`'s header gives them, and [`verdict`] is
//! the one place that translation happens: 0 verified; 1 a refusal (the gate's own `FAIL` line is in the output this
//! command forwards); 2, 125, 126, 127 and 137 are "could not verify", with the sentence the gate or Docker wrote;
//! and 3 is `scripts/check-crate-set.sh`'s own answer, reached through `scripts/verify.sh`'s preflight before its
//! first phase — a **host act** rather than a broken registry, and the refusal [`Verdict::StaleCrateSet`] produces
//! names the repair.
//! A `SKIP ` line inside the gate measures nothing, so it is counted and the report carries how many there were —
//! a green run with skips is still green, and saying so is the difference between a proof and a claim.
//!
//! # Why ten gigabytes are asked for before the gate starts
//!
//! The gate writes into the candidate tree it builds. ADR-0107 measured that in a session's shape: `cargo build
//! --locked`, `cargo clippy --all-targets` and `cargo test --no-run` write 9.2 GB of `target/`, and both musl release
//! builds take it to 9.6 GB, with `dist/` and the web app's `node_modules` on top of that. So [`NEEDED_GB`] is ten,
//! it is a **constant** and not a config key — it is a fact about what this build produces, not a preference an
//! operator can have — and the check runs *before* the gate because a run that fills the disk takes the machine's
//! database down with it.
//!
//! # What this module does not decide
//!
//! **The routes and the page.** `GET /api/update` answers [`Report`]'s own JSON — the file this module writes — plus
//! the log's last lines, which is why the type is here and not in the command surface. `POST /api/update` starts this
//! command as a transient unit and writes nothing itself: the report is the job's, so the route cannot lie about a
//! job it did not start. The page is a later change's.
//!
//! **The launcher.** [`crate::update`] starts a command line as a transient user unit and has no opinion about what
//! runs behind it; a route passes this command's argv there. Nothing here asks systemd anything.
//!
//! **A second verification.** The gate is the candidate tree's own script, read from `<source>/scripts/`, so this
//! command cannot verify a tree that does not carry one and never falls back to a gate it happens to be beside: the
//! proof a promotion rests on is the proof of the code being promoted.

use std::collections::VecDeque;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::builder;
use crate::candidate::{self, Candidate};
use crate::config::Config;
use crate::deploy::{self, Root};
use crate::disk;
use crate::images;
use crate::promote::{self, Steps};
use crate::remote;

/// The verb, and the prefix every line this command prints carries.
///
/// One spelling: the pipeline's own lines take it as [`promote::Options::verb`], and the machine a promotion asks
/// ([`crate::promote::Real::prefixed`]) takes the same value, so no line of an `update` run can say `upgrade` and no
/// line of an `upgrade` run can say `update`.
pub const VERB: &str = "update";

/// How much free space a pass needs on the candidate's filesystem, in GB, before the gate starts (ADR-0107).
pub const NEEDED_GB: u64 = 10;

/// The variable the gate's wrapper reads to pick the image it runs in.
///
/// `scripts/verify-in-container.sh`'s own knob, named here because this command sets it and the gate does not read it
/// from anywhere else: its default is the tag of the tree's `Cargo.toml`, which in a candidate tree names the
/// *candidate's* version — and the candidate's image is the one thing that cannot exist before the candidate does.
const IMAGE: &str = "HOLOSMITH_VERIFY_IMAGE";

/// The candidate tree's own wrapper, relative to `[update] source` (ADR-0109).
const VERIFY: &str = "scripts/verify-in-container.sh";

/// How a `SKIP` line reads in the gate's output: the marker at column 0, then the case it belongs to.
const SKIP: &str = "SKIP ";

/// How many of the gate's last non-blank lines a refusal carries.
///
/// "Few" for the reason a refusal lists few dirty entries: a refusal names what to look at, and the whole log is the
/// gate's, in the output this command already forwarded.
const TAIL: usize = 8;

/// What the command line asked for.
#[derive(Debug, Clone)]
pub struct Options {
    /// The file the run's report is written to (`--report`), when a caller asked for one.
    pub report: Option<PathBuf>,
    /// How long the promotion's drain may wait for the turns that are running (the flag's, default `10m`).
    pub wait: Duration,
}

/// The whole pass: its four steps, the report, and the exit code its endings carry.
///
/// Exit 0 only for a candidate that is **live**; 1 for every other ending, because a job's caller reads *which* one
/// from the report and not from a status every shell flattens.
pub fn run(
    config: &Config,
    root: &Root,
    options: &Options,
    steps: &dyn Steps,
    tools: &deploy::Tools,
) -> ExitCode {
    let mut report = Report::start();
    // The seed, written **before** the first step: a caller that polls `--report` reads a file from the moment the run
    // exists, which is what makes "is a promotion running, and since when" a question the file answers rather than
    // one only `systemctl` could. It is overwritten by the ending below; a run that is killed leaves `running` in
    // place until the next run writes its own seed, which is the one lie this file can hold and the reason the state
    // is named for the run rather than for the machine.
    if let Some(path) = &options.report
        && let Err(said) = report.write(path)
    {
        eprintln!("holosmith: {VERB}: {said}");
        return ExitCode::FAILURE;
    }
    let outcome = pass(config, root, options, steps, tools, &mut report);
    report.state = outcome.state();
    report.detail = outcome.said().map(str::to_string);
    let code = match &outcome {
        Outcome::Live => ExitCode::SUCCESS,
        other => {
            eprintln!("holosmith: {VERB}: {}", other.said().unwrap_or_default());
            ExitCode::FAILURE
        }
    };
    // Decision 2 of the ticket: the report is written for every ending, so a caller reads one file whatever happened
    // — and a run that cannot write it is a failed run, because `--report` is the whole of what it was asked for.
    if let Some(path) = &options.report {
        if let Err(said) = report.write(path) {
            eprintln!("holosmith: {VERB}: {said}");
            return ExitCode::FAILURE;
        }
        println!("{VERB}: report: {}", path.display());
    }
    code
}

/// The four steps, in the order the acceptance names them, appending what each one ended as to `report`.
fn pass(
    config: &Config,
    root: &Root,
    options: &Options,
    steps: &dyn Steps,
    tools: &deploy::Tools,
    report: &mut Report,
) -> Outcome {
    // 1. The candidate: one commit, and the reason it is not promotable when there is none.
    let candidate = match candidate::resolve(config) {
        Ok(candidate) => candidate,
        Err(refusal) => return refused(report, "candidate", refusal.to_string()),
    };
    println!(
        "{VERB}: candidate: {} at {}, tagged {}",
        candidate.source.display(),
        abbreviated(&candidate.commit),
        candidate.tag
    );
    report.candidate = Some(candidate.clone());
    report.steps.push(StepReport::new("candidate", "resolved"));

    // 2. Room to work in, before the gate writes ten gigabytes into that tree (ADR-0107, [`NEEDED_GB`]).
    let available = match disk::measure(Path::new(disk::DF), &candidate.source) {
        Ok(available) => available,
        Err(error) => return refused(report, "candidate", unmeasured(&candidate.source, &error)),
    };
    if disk::verdict(available, NEEDED_GB) == disk::Verdict::Short {
        return refused(
            report,
            "candidate",
            short_room(&candidate.source, available, NEEDED_GB),
        );
    }

    // 3. The gate: the candidate tree's own wrapper, in the image the running release deployed.
    let script = candidate.source.join(VERIFY);
    if !script.is_file() {
        return refused(report, "candidate", no_wrapper(&script));
    }
    let image = images::agent_image();
    println!(
        "{VERB}: gate: {} ({IMAGE}={image}); {available} GB free on that filesystem, {NEEDED_GB} needed",
        script.display()
    );
    let gate = match verify(&script, &candidate.source, &image) {
        Ok(gate) => gate,
        Err(said) => return could_not_verify(report, said),
    };
    let verdict = verdict(gate.status.code());
    println!(
        "{VERB}: gate: {} ({}){}",
        verdict.word(),
        status_text(&gate.status),
        skips_clause(gate.skips)
    );
    report.gate = Some(GateReport {
        command: script,
        image,
        exit_code: gate.status.code(),
        ending: verdict.word().to_string(),
        skips: gate.skips,
    });
    report.steps.push(StepReport::new("gate", verdict.word()));
    match verdict {
        Verdict::Verified => {}
        Verdict::Refused => return Outcome::Refused(gate_sentence(verdict, &gate.tail)),
        Verdict::StaleCrateSet | Verdict::Unverified(_) => {
            return Outcome::CouldNotVerify(gate_sentence(verdict, &gate.tail));
        }
    }

    // 4. The artifact the gate's build phase wrote — and a green gate that left none is a refusal, not a promotion
    //    attempt on whatever file happens to be there.
    let artifact = match built(&candidate.source) {
        Ok(artifact) => artifact,
        Err(said) => return refused(report, "artifact", said),
    };
    println!(
        "{VERB}: promote: {} (sha256 {})",
        artifact.path.display(),
        abbreviated(&artifact.digest)
    );
    let promotion = promote::Options {
        candidate: artifact.path.clone(),
        review: Some(promote::Review::Verified),
        verb: VERB,
        wait: options.wait,
        window: promote::WINDOW,
    };
    report.artifact = Some(artifact);
    match promote::execute(config, root, &promotion, steps, tools) {
        Ok(()) => {
            report.steps.push(StepReport::new("promotion", "live"));
            Outcome::Live
        }
        Err(said) => {
            report.steps.push(StepReport::new("promotion", "failed"));
            Outcome::Failed(said)
        }
    }
}

/// The artifact a green gate left, or the refusal that names the two files and the command that writes them.
fn built(source: &Path) -> Result<ArtifactReport, String> {
    let triple = builder::default_target()?;
    let directory = source.join("dist").join(triple);
    let path = directory.join(builder::BINARY);
    let digest = directory.join(builder::DIGEST);
    if !path.is_file() || !digest.is_file() {
        return Err(format!(
            "the gate is green and it left no artifact to promote: {} and {} are not both there, and the gate's own \
             build phase writes them (`scripts/build.sh`, which `scripts/verify.sh` runs unless it is given \
             `--no-build`). Nothing was promoted",
            path.display(),
            digest.display()
        ));
    }
    let recorded = std::fs::read_to_string(&digest).map_err(|error| {
        format!(
            "{} could not be read ({error}): it is the digest the build wrote beside {}, and a promotion reads both \
             — nothing was promoted",
            digest.display(),
            path.display()
        )
    })?;
    let digest = remote::digest_of(&recorded).ok_or_else(|| {
        format!(
            "{} holds no sha256sum line, so the digest of {} cannot be checked: `scripts/build.sh` writes \
             `<digest>  {}` there. Nothing was promoted",
            digest.display(),
            path.display(),
            builder::BINARY
        )
    })?;
    Ok(ArtifactReport { path, digest })
}

/// The gate's `SKIP ` line count over a listing: the rule [`Audit`] applies line by line, in one expression so the
/// test that pins it and the counter an `update` run uses cannot drift apart.
#[cfg(test)]
fn skips(lines: &[&str]) -> u64 {
    lines.iter().filter(|line| is_skip(line)).count() as u64
}

/// Whether one line of the gate's output is a `SKIP`: the marker at column 0, which is where `scripts/verify.sh`
/// prints it.
///
/// Column 0 rather than "somewhere in the line" is the whole rule: a case's own output can quote the word, and a
/// counter that read those would report skips the gate never made.
fn is_skip(line: &str) -> bool {
    line.starts_with(SKIP)
}

/// The clause a printed line and a refusal share when the gate skipped something: a `SKIP` is not a pass.
fn skips_clause(skips: u64) -> String {
    match skips {
        0 => String::new(),
        1 => ", 1 SKIP (a line the gate measured nothing for)".to_string(),
        count => format!(", {count} SKIP (lines the gate measured nothing for)"),
    }
}

/// What the gate's exit status says about the tree.
///
/// One arm per code `scripts/verify-in-container.sh`'s header names, plus the two a header cannot enumerate — a
/// signal, and a status that is none of them. The translation is a function so that a printed line, the report and a
/// refusal cannot disagree about what 3 means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 0: the tree is proven, and the artifact the gate's build phase wrote is the candidate.
    Verified,
    /// 1: the gate ran and the tree failed it, and the gate's own `FAIL` line says where.
    Refused,
    /// 3: the candidate's `Cargo.lock` names a crate set the running release's `agent-image` does not vendor.
    ///
    /// `scripts/check-crate-set.sh`'s own exit, reached through `scripts/verify.sh`'s preflight — a **host act**
    /// rather than a code fix: the image is not stale, it is the running release's, and a container has no registry
    /// to fetch a new crate from (ADR-0110).
    StaleCrateSet,
    /// Any other status: the gate never reached a verdict about the tree at all.
    Unverified(RunFailure),
}

impl Verdict {
    /// The word the printed line, the report's step ending and the gate's own ending carry.
    fn word(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Refused => "refused",
            Self::StaleCrateSet => "stale crate set",
            Self::Unverified(_) => "could not verify",
        }
    }
}

/// Which non-verdict the gate's exit status is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFailure {
    /// 2: the wrapper refused to start, or `scripts/verify.sh` was handed an argument it does not know.
    Wrapper,
    /// 125: `docker run` itself failed.
    Docker,
    /// 126: a command inside the container is not executable.
    NotExecutable,
    /// 127: a command the container runs is not there.
    NotFound,
    /// 137: the kernel killed the container.
    Killed,
    /// The gate was killed by a signal, so it has no status at all.
    Signalled,
    /// A status that is none of 0, 1, 2, 3, 125, 126, 127 or 137.
    Other(i32),
}

impl fmt::Display for RunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wrapper => f.write_str(
                "`scripts/verify-in-container.sh` refused to start, or `scripts/verify.sh` was handed an argument it \
                 does not know (exit 2), and the gate's own line says which",
            ),
            Self::Docker => f.write_str("`docker run` itself failed (exit 125), so no container ran the gate"),
            Self::NotExecutable => {
                f.write_str("a command the gate's container runs is not executable (exit 126)")
            }
            Self::NotFound => f.write_str("a command the gate's container runs is not there (exit 127)"),
            Self::Killed => f.write_str(
                "the kernel killed the gate's container (exit 137), which is a signal and not a verdict about the \
                 tree",
            ),
            Self::Signalled => {
                f.write_str("the gate was killed by a signal, so it reached no verdict at all")
            }
            Self::Other(code) => write!(
                f,
                "the gate answered {code}, which is none of 0, 1, 2, 3, 125, 126, 127 or 137, so it reached no \
                 verdict this command can read"
            ),
        }
    }
}

/// What one exit status means: the translation `scripts/verify-in-container.sh`'s header promises.
pub fn verdict(code: Option<i32>) -> Verdict {
    match code {
        Some(0) => Verdict::Verified,
        Some(1) => Verdict::Refused,
        Some(3) => Verdict::StaleCrateSet,
        Some(2) => Verdict::Unverified(RunFailure::Wrapper),
        Some(125) => Verdict::Unverified(RunFailure::Docker),
        Some(126) => Verdict::Unverified(RunFailure::NotExecutable),
        Some(127) => Verdict::Unverified(RunFailure::NotFound),
        Some(137) => Verdict::Unverified(RunFailure::Killed),
        Some(code) => Verdict::Unverified(RunFailure::Other(code)),
        None => Verdict::Unverified(RunFailure::Signalled),
    }
}

/// The sentence a gate that did not verify the tree refuses with, and the one the report carries as its `detail`.
fn gate_sentence(verdict: Verdict, tail: &[String]) -> String {
    let said = match verdict {
        Verdict::Refused => format!(
            "the gate ran and the tree failed it (exit 1), so nothing was promoted: {} Its `FAIL` line is in the \
             output above, and the gate's log is the whole record",
            NOTHING_ASKED
        ),
        Verdict::StaleCrateSet => format!(
            "the gate answered 3: the candidate's `Cargo.lock` names a crate set the running release's \
             `agent-image` does not vendor, which is a host act and not a broken registry — fetch that lock on a host \
             that has one (`cargo fetch --locked` in the candidate tree), build and deploy the candidate's own image \
             from it, and run this again. {NOTHING_ASKED}"
        ),
        Verdict::Unverified(failure) => {
            format!(
                "the gate could not verify this tree, so nothing was promoted: {failure}. {NOTHING_ASKED}"
            )
        }
        Verdict::Verified => String::new(),
    };
    with_tail(said, tail)
}

/// The clause every gate refusal ends with: what the machine was not asked, and why that is the point.
const NOTHING_ASKED: &str =
    "Nothing was asked of the machine — no smoke run, no drain, no backup, nothing switched.";

/// `said` followed by the gate's last lines, which is where a refusal's own reason is.
fn with_tail(said: String, tail: &[String]) -> String {
    if tail.is_empty() {
        return said;
    }
    format!("{said}\nits last lines were:\n  {}", tail.join("\n  "))
}

/// The refusal for a candidate tree with no gate wrapper in it.
fn no_wrapper(script: &Path) -> String {
    format!(
        "{} is not there: a pass of the gate runs the candidate tree's own wrapper, so `[update] source` has to point \
         at a checkout of this repository. Nothing was run",
        script.display()
    )
}

/// The refusal for a filesystem with no room for the gate's writes.
fn short_room(source: &Path, available: u64, needed: u64) -> String {
    format!(
        "{} has {available} GB free on its filesystem and a pass of the gate needs {needed} GB there: it builds the \
         web app, both musl artifacts and every test binary into that tree. Nothing was run — free {} GB on that \
         filesystem, or point `[update] source` at a checkout on one that has it, and run this again",
        source.display(),
        needed.saturating_sub(available)
    )
}

/// The refusal for a free-space question the machine could not answer.
fn unmeasured(source: &Path, error: &disk::DiskError) -> String {
    format!(
        "the free space on the filesystem that holds {} could not be measured ({error}), so this command cannot say \
         whether the gate has room for its writes and nothing was run: install coreutils, or point `[update] source` \
         at a checkout on a filesystem `df -Pk` can read",
        source.display()
    )
}

/// One ending: what the pass came to, and the sentence a caller reads.
///
/// Four because the machine's state and the caller's next act differ: a refusal asked the machine nothing, a gate
/// that could not verify never judged the tree, and a failed promotion ran a pipeline that may have left the machine
/// draining.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// The candidate answered `/api/health` as a process younger than the restart: it is live.
    Live,
    /// A step before the gate's verdict refused, and nothing was asked of the machine.
    Refused(String),
    /// The gate never reached a verdict about this tree.
    CouldNotVerify(String),
    /// The promotion ran and the candidate did not become live.
    Failed(String),
}

impl Outcome {
    fn state(&self) -> State {
        match self {
            Self::Live => State::Live,
            Self::Refused(_) => State::Refused,
            Self::CouldNotVerify(_) => State::CouldNotVerify,
            Self::Failed(_) => State::Failed,
        }
    }

    fn said(&self) -> Option<&str> {
        match self {
            Self::Live => None,
            Self::Refused(said) | Self::CouldNotVerify(said) | Self::Failed(said) => Some(said),
        }
    }
}

/// A step that did not get past itself: its report entry ends `refused`, and the run ends here.
fn refused(report: &mut Report, step: &str, said: String) -> Outcome {
    report.steps.push(StepReport::new(step, "refused"));
    Outcome::Refused(said)
}

/// The gate could not be started at all, so it has no status and no report entry of its own.
fn could_not_verify(report: &mut Report, said: String) -> Outcome {
    report.steps.push(StepReport::new("gate", "did not run"));
    Outcome::CouldNotVerify(said)
}

/// One gate run: how it ended, how many `SKIP ` lines it printed, and the tail of what it said.
struct Gate {
    status: ExitStatus,
    skips: u64,
    tail: Vec<String>,
}

/// Run the candidate's own gate wrapper, forwarding its output line by line as it arrives.
///
/// Forwarding as it arrives rather than buffering the whole run: the gate takes three quarters of an hour and its
/// output is the only feedback a job's log has. The lines are also read for the two facts a refusal and the report
/// need — the `SKIP ` count, and the last few lines — which is why it is a pipe and not an inherited stream.
fn verify(script: &Path, source: &Path, image: &str) -> Result<Gate, String> {
    let mut child = Command::new(script)
        // The candidate tree, the same `WorkingDirectory=` a job started from a request gets ([`crate::update`]):
        // the gate derives its own root from `BASH_SOURCE`, and a job's relative paths are this tree's either way.
        .current_dir(source)
        .env(IMAGE, image)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "{} could not be run ({error}): a pass of the gate runs the candidate tree's own wrapper, so that file \
                 has to be executable. Nothing was run",
                script.display()
            )
        })?;
    let Some(stdout) = child.stdout.take() else {
        return Err(format!(
            "the output of {} could not be captured, so this run could not read the gate's verdict: a pass of the \
             gate needs the gate's own lines",
            script.display()
        ));
    };
    let Some(stderr) = child.stderr.take() else {
        return Err(format!(
            "the output of {} could not be captured, so this run could not read the gate's verdict: a pass of the \
             gate needs the gate's own lines",
            script.display()
        ));
    };
    let audit = Arc::new(Audit::default());
    // One reader per stream, because a pipe fills: a gate that wrote a full buffer of stderr while this thread waited
    // on stdout would deadlock, and the gate writes both.
    let counted = Arc::clone(&audit);
    let forwarded = std::thread::spawn(move || {
        forward(BufReader::new(stdout), std::io::stdout(), &counted, true)
    });
    let quiet = Arc::clone(&audit);
    let relayed = std::thread::spawn(move || {
        forward(BufReader::new(stderr), std::io::stderr(), &quiet, false)
    });
    let status = child.wait().map_err(|error| {
        format!(
            "{} could not be waited for ({error}), so this run cannot say how the gate ended",
            script.display()
        )
    })?;
    let _ = forwarded.join();
    let _ = relayed.join();
    Ok(Gate {
        status,
        skips: audit.skips(),
        tail: audit.tail(),
    })
}

/// What the gate's output is read for as it arrives.
///
/// Shared by the two reader threads, because the tail a refusal carries is the last lines the gate printed in
/// whichever order they came out: stdout and stderr are one stream to a reader of the log, which is what the job's
/// file is.
#[derive(Debug, Default)]
struct Audit {
    skips: AtomicU64,
    tail: Mutex<VecDeque<String>>,
}

impl Audit {
    /// One line, as it arrives: counted when it is the gate's `SKIP ` marker, kept when it is not blank.
    fn record(&self, line: &str, count: bool) {
        if count && is_skip(line) {
            self.skips.fetch_add(1, Ordering::Relaxed);
        }
        if line.trim().is_empty() {
            return;
        }
        let mut tail = self
            .tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tail.len() == TAIL {
            tail.pop_front();
        }
        tail.push_back(line.to_string());
    }

    fn skips(&self) -> u64 {
        self.skips.load(Ordering::Relaxed)
    }

    fn tail(&self) -> Vec<String> {
        self.tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

/// Copy one of the gate's streams to `sink`, one line at a time.
///
/// A write that fails is not a reason to stop reading: the gate would then fill its pipe and block, and a run whose
/// output nobody wanted any more would never end. Only a read that fails ends the loop.
fn forward(
    mut pipe: impl BufRead,
    mut sink: impl Write,
    audit: &Audit,
    count: bool,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        if pipe.read_until(b'\n', &mut buffer)? == 0 {
            return Ok(());
        }
        let raw = String::from_utf8_lossy(&buffer);
        let line = raw.trim_end_matches(['\n', '\r']);
        audit.record(line, count);
        let _ = writeln!(sink, "{line}");
        let _ = sink.flush();
    }
}

/// The status as a refusal and a printed line name it: `exit 0`, or `signal 9` when there is no code at all.
fn status_text(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit {code}"),
        None => status.to_string(),
    }
}

/// The first twelve characters of a commit or a digest: enough to recognise, short enough to read.
fn abbreviated(hex: &str) -> &str {
    hex.get(..12).unwrap_or(hex)
}

/// How a pass stands: the report's `state`, and the one word a caller switches on.
///
/// Five, and the first is not an ending: [`State::Running`] is the seed [`run`] writes before the first step, so a
/// caller polling the file can tell a run under way from a run that ended. The other four are the endings, and each
/// is a different next act for whoever reads it.
///
/// Serialized through [`State::word`] rather than by a `rename` attribute, so the JSON and the sentence a failed
/// report write carries cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The run is under way: this object is the seed, and an ending will replace it.
    Running,
    /// The candidate answered `/api/health` as a process younger than the restart.
    Live,
    /// A step refused, or the run ended before one could, and the machine was asked nothing.
    Refused,
    /// The gate never reached a verdict about the tree.
    CouldNotVerify,
    /// The promotion ran and the candidate did not become live.
    Failed,
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Live => "live",
            Self::Refused => "refused",
            Self::CouldNotVerify => "could-not-verify",
            Self::Failed => "failed",
        }
    }
}

impl Serialize for State {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.word())
    }
}

/// One pass, as a job's caller reads it: the object `--report` writes.
///
/// The fields are the ones `GET /api/update` answers, which is why the type lives in the library and not beside the
/// command: a route that answered a second shape would be a second spelling of one run. The route adds one key the
/// report cannot hold — the log's last lines, which are a file's and not this value's.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// How the pass stands: under way, or how it ended.
    pub state: State,
    /// When the run began, in unix seconds — the instant [`run`] wrote this object's first version.
    ///
    /// In the file rather than only in the answer to the request that started the run, because the file outlives
    /// both the request and the restart a promotion performs: `GET /api/update` keeps answering when the job
    /// started after `serve` has been replaced.
    pub since: i64,
    /// The commit the pass was about, when `[update] source` named one.
    pub candidate: Option<Candidate>,
    /// The gate, when it ran.
    pub gate: Option<GateReport>,
    /// The artifact the gate's build phase left, when it left one.
    pub artifact: Option<ArtifactReport>,
    /// One entry per step, in the order the pass reached them — so a caller can see where a refusal happened
    /// without reading the log.
    pub steps: Vec<StepReport>,
    /// The sentence the ending carries: what refused, or why the gate could not verify. `null` for a live
    /// candidate.
    pub detail: Option<String>,
}

impl Report {
    /// A report with nothing in it yet: the run is under way, and it began now.
    ///
    /// Its state is `running` and its `since` is this instant: nothing is claimed live until the health poll says so,
    /// and every path out of [`run`] sets the state from the [`Outcome`] before the object is written for the last
    /// time.
    fn start() -> Self {
        Self {
            state: State::Running,
            since: crate::clock::unix_now(),
            candidate: None,
            gate: None,
            artifact: None,
            steps: Vec::new(),
            detail: None,
        }
    }

    /// The report of an ending that happened before [`run`] could begin one: the config could not be loaded, so no
    /// step was reached and nothing was asked of the machine.
    ///
    /// It exists because `--report` is written for **every** ending, and this one is the caller's worst case for a
    /// file that is only sometimes there: a job whose config is broken would otherwise leave nothing behind, and a
    /// polling caller cannot tell a missing report from a crashed run.
    pub fn refused(detail: &str) -> Self {
        Self {
            state: State::Refused,
            detail: Some(detail.to_string()),
            ..Self::start()
        }
    }

    /// Write the object to `path`, as one JSON object and a newline.
    ///
    /// Through `<path>.tmp` and a rename, which is the shape every other artifact here takes (`scripts/build.sh`'s
    /// digest, `release.rs`'s binary): the caller this file exists for **polls** it, and a write in place leaves a
    /// window in which it reads half an object.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|error| {
            format!(
                "the report could not be rendered as JSON ({error}), so `--report` wrote nothing"
            )
        })?;
        let said = |error: std::io::Error| {
            format!(
                "the run ended {} and its report could not be written to {} ({error}): `--report` is the whole of \
                 what a caller reads a run out of, so a run that cannot write it is a failed one",
                self.state.word(),
                path.display()
            )
        };
        let mut beside = path.as_os_str().to_os_string();
        beside.push(".tmp");
        let temporary = PathBuf::from(beside);
        std::fs::write(&temporary, format!("{text}\n")).map_err(said)?;
        std::fs::rename(&temporary, path).map_err(said)?;
        Ok(())
    }
}

/// The gate's own ending, as the report carries it.
#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    /// The script this command ran, and nothing else: the gate takes no argument from here.
    pub command: PathBuf,
    /// `HOLOSMITH_VERIFY_IMAGE`, which is the image the gate ran in.
    pub image: String,
    /// The exit status, or `null` when a signal killed the gate.
    pub exit_code: Option<i32>,
    /// How the status read: `verified`, `refused`, `stale crate set` or `could not verify`.
    pub ending: String,
    /// How many `SKIP ` lines the gate printed: a `SKIP` measures nothing, and a green run says so.
    pub skips: u64,
}

/// The artifact the gate left, as the report carries it.
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactReport {
    /// `<source>/dist/<triple>/holosmith`: what the promotion was handed.
    pub path: PathBuf,
    /// The digest its `.sha256` file records.
    pub digest: String,
}

/// One step's ending.
#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    /// The step: `candidate`, `gate`, `artifact` or `promotion`.
    pub step: String,
    /// How it ended: `resolved`, `verified`, `refused`, `live`, `failed`, …
    pub ending: String,
}

impl StepReport {
    fn new(step: &str, ending: &str) -> Self {
        Self {
            step: step.to_string(),
            ending: ending.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header's table, measured: a code is a verdict here or nowhere else.
    #[test]
    fn the_gates_exit_codes_keep_their_meanings() {
        assert_eq!(verdict(Some(0)), Verdict::Verified);
        assert_eq!(verdict(Some(1)), Verdict::Refused);
        assert_eq!(verdict(Some(3)), Verdict::StaleCrateSet);
        for (code, failure) in [
            (2, RunFailure::Wrapper),
            (125, RunFailure::Docker),
            (126, RunFailure::NotExecutable),
            (127, RunFailure::NotFound),
            (137, RunFailure::Killed),
        ] {
            assert_eq!(
                verdict(Some(code)),
                Verdict::Unverified(failure),
                "exit {code} does not read as {failure:?}"
            );
        }
        // A signal has no code at all, and a code outside the header is still not a verdict.
        assert_eq!(verdict(None), Verdict::Unverified(RunFailure::Signalled));
        assert_eq!(
            verdict(Some(70)),
            Verdict::Unverified(RunFailure::Other(70))
        );
    }

    /// The three sentences a caller acts on, one of them the host act exit 3 stands for.
    #[test]
    fn a_gate_that_did_not_verify_refuses_with_the_sentence_its_code_means() {
        let tail = vec!["check-crate-set: names a different Cargo.lock".to_string()];

        let stale = gate_sentence(Verdict::StaleCrateSet, &tail);
        assert!(stale.contains("`Cargo.lock`"), "{stale}");
        assert!(stale.contains("host act"), "{stale}");
        assert!(stale.contains("cargo fetch --locked"), "{stale}");
        assert!(
            !stale.contains("not found in the registry index"),
            "the refusal repeats the misreading it exists to prevent: {stale}"
        );
        assert!(
            stale.contains("check-crate-set"),
            "the gate's line is gone: {stale}"
        );

        let red = gate_sentence(Verdict::Refused, &[]);
        assert!(red.contains("failed it"), "{red}");
        assert!(red.contains("no drain"), "{red}");
        assert!(
            !red.contains("its last lines were"),
            "a tail that was not there was invented: {red}"
        );

        let docker = gate_sentence(Verdict::Unverified(RunFailure::Docker), &[]);
        assert!(docker.contains("`docker run` itself failed"), "{docker}");
        assert!(docker.contains("no smoke run"), "{docker}");
    }

    /// Column 0, and nothing else: a case that quotes the word is not a skip the gate made.
    #[test]
    fn the_skip_count_reads_the_marker_at_the_start_of_a_line() {
        let output = [
            "case_thing: ok",
            "SKIP case_thing: no Docker daemon answers",
            "  SKIP indented: a case's own output",
            "SKIPPED: a word that starts with it",
            "",
            "SKIP case_other: no image pi-agent:local (HOLOSMITH_TEST_IMAGE names another)",
        ];
        assert_eq!(skips(&output), 2);
        assert_eq!(skips(&[]), 0);
        assert!(skips_clause(0).is_empty());
        assert!(skips_clause(2).contains("2 SKIP"));
        assert!(skips_clause(1).contains("1 SKIP"));
    }

    /// The report is one object with the fields `GET /api/update` answers, and its state is the word a caller
    /// switches on.
    #[test]
    fn the_report_is_one_json_object_a_route_can_answer() {
        let report = Report {
            state: State::CouldNotVerify,
            since: 1_700_000_000,
            candidate: Some(Candidate {
                source: PathBuf::from("/src/holosmith"),
                commit: "0".repeat(40),
                tag: "v1.2.3-4-gabc".to_string(),
            }),
            gate: Some(GateReport {
                command: PathBuf::from("/src/holosmith/scripts/verify-in-container.sh"),
                image: "agent-image:0.1.0".to_string(),
                exit_code: Some(125),
                ending: "could not verify".to_string(),
                skips: 3,
            }),
            artifact: None,
            steps: vec![
                StepReport::new("candidate", "resolved"),
                StepReport::new("gate", "could not verify"),
            ],
            detail: Some("the gate could not verify this tree".to_string()),
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).expect("a report serializes"))
                .expect("the report is one JSON object");

        assert_eq!(value["state"], "could-not-verify");
        assert_eq!(value["since"], 1_700_000_000);
        assert_eq!(value["candidate"]["source"], "/src/holosmith");
        assert_eq!(value["candidate"]["tag"], "v1.2.3-4-gabc");
        assert_eq!(
            value["gate"]["command"],
            "/src/holosmith/scripts/verify-in-container.sh"
        );
        assert_eq!(value["gate"]["image"], "agent-image:0.1.0");
        assert_eq!(value["gate"]["exit_code"], 125);
        assert_eq!(value["gate"]["skips"], 3);
        assert!(value["artifact"].is_null());
        assert_eq!(value["steps"][1]["step"], "gate");
        assert_eq!(value["steps"][1]["ending"], "could not verify");
        assert_eq!(value["detail"], "the gate could not verify this tree");

        // Every state has the word the prose uses, so the report and a sentence cannot spell one twice.
        for (state, word) in [
            (State::Running, "running"),
            (State::Live, "live"),
            (State::Refused, "refused"),
            (State::CouldNotVerify, "could-not-verify"),
            (State::Failed, "failed"),
        ] {
            assert_eq!(
                serde_json::to_string(&state).expect("a state is a string"),
                format!("\"{word}\"")
            );
            assert_eq!(state.word(), word);
        }
    }

    /// A report with nothing in it yet is the seed: `running`, dated now, and nothing claimed about a candidate or a
    /// gate. It is what makes "is a promotion running, and since when" a question the file answers before the first
    /// step has run — and the refusal arm keeps the field rather than dropping it, because every ending names when the
    /// run that ended began.
    #[test]
    fn a_fresh_report_is_a_running_seed_dated_now() {
        let before = crate::clock::unix_now();
        let seed = Report::start();
        let after = crate::clock::unix_now();
        assert_eq!(seed.state, State::Running);
        assert!(
            (before..=after).contains(&seed.since),
            "the seed's `since` is not the instant it was made: {} outside {before}..={after}",
            seed.since
        );
        assert!(seed.candidate.is_none() && seed.gate.is_none() && seed.artifact.is_none());
        assert!(seed.steps.is_empty() && seed.detail.is_none());

        let refused = Report::refused("the config could not be read");
        assert_eq!(refused.state, State::Refused);
        assert!(
            (before..=after).contains(&refused.since),
            "a refusal before the run began has no `since`: {}",
            refused.since
        );
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&refused).expect("a report serializes"))
                .expect("the report is one JSON object");
        assert_eq!(value["state"], "refused");
        assert_eq!(value["since"], refused.since);
    }

    /// The one refusal that names two numbers: the space it found and the space the run needs.
    #[test]
    fn a_filesystem_with_no_room_is_refused_with_both_numbers() {
        let said = short_room(Path::new("/src/holosmith"), 3, NEEDED_GB);
        assert!(said.contains("/src/holosmith"), "{said}");
        assert!(said.contains("3 GB free"), "{said}");
        assert!(said.contains(&format!("needs {NEEDED_GB} GB")), "{said}");
        assert!(
            said.contains("free 7 GB"),
            "the sentence does not say what to free: {said}"
        );
    }

    /// The report goes through `<path>.tmp` and a rename, so the caller that polls this file never reads half an
    /// object — and what it reads is one JSON object and a newline.
    #[test]
    fn a_report_is_written_through_a_temporary_and_ends_with_a_newline() {
        let directory = crate::sql::guard::TempDir::new("pass-report");
        let path = directory.path().join("report.json");
        Report::refused("the config could not be read")
            .write(&path)
            .expect("the report is written");

        let text = std::fs::read_to_string(&path).expect("the report is there");
        assert!(
            text.ends_with("}\n"),
            "the file does not end with a newline: {text:?}"
        );
        let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON object");
        assert_eq!(value["state"], "refused");
        assert_eq!(value["steps"].as_array().map(Vec::len), Some(0));
        assert!(
            !directory.path().join("report.json.tmp").exists(),
            "the temporary was left beside the report"
        );
    }

    /// A green gate that wrote no artifact is a refusal naming both files and the command that writes them: a
    /// promotion of whatever a `dist/` happens to hold is the review-artefact mistake this ticket refuses.
    #[test]
    fn a_green_gate_that_left_no_artifact_names_the_two_files_and_the_command() {
        let directory = crate::sql::guard::TempDir::new("pass-no-artifact");
        let said = built(directory.path()).expect_err("an empty tree holds no artifact");
        assert!(said.contains("no artifact"), "{said}");
        assert!(said.contains(builder::BINARY), "{said}");
        assert!(said.contains(builder::DIGEST), "{said}");
        assert!(said.contains("scripts/build.sh"), "{said}");
    }
}
