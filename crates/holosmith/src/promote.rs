//! Spec item 10's promotion pipeline: the eight steps below, and the lines they print carrying the run's own verb.
//!
//! `holosmith upgrade <binary>` is the command an operator hands a file to; `holosmith update` is the one a job runs,
//! which proves the tree with the gate first and then calls this pipeline ([`crate::pass`]). The pipeline itself is
//! one, which is what ADR-0082 D1 insists on.
//!
//! Item 10's six steps, in the order the acceptance names them, are this module's one function:
//!
//! 1. **the review gate acknowledged** — `--acknowledge-review`, printed with the digest and the version it
//!    acknowledges, because item 12's diff review is a human act and this flag is the record of it. Without it the
//!    command refuses, and there is no other path in this command to the drain (ADR-0082 D2).
//! 2. **the host smoke test** — `smoke::run` in process, before the drain: the candidate is proven while the machine
//!    still serves the version it is replacing, and a candidate that fails is one nothing was drained for.
//! 3. **the images**, read and rebuilt only when the candidate's labels say so. They are not one of the spec's
//!    steps, and their place is the one that fails safely: before the drain, so a build the daemon refuses aborts
//!    with nothing drained, nothing backed up and nothing switched.
//! 4. **the drain** — `drain --wait`, and its three endings are three different things: `drained` proceeds,
//!    `expired` refuses (a turn outlasted the wait), and `state: already draining` refuses *before* waiting out a
//!    drain this promotion did not start (acceptance bullet 5's first half).
//! 5. **the database backup** — `VACUUM INTO backups/app-<ts>.db`, five kept, over the read-only opener.
//! 6. **the release, the symlink and the restart** — the candidate's bytes into `releases/<tag>/`, the unit's
//!    `ExecStart` through `current`, the atomic switch, `systemctl --user restart`, and then a 20-second health poll
//!    that looks for a process **younger than the restart** answering with the candidate's version. If it does not
//!    answer, the previous link target goes back, the service is restarted again and the poll runs again — and the
//!    database is not restored, because expand/contract is what makes the switch back safe (ADR-0047).
//!
//! # Why the steps are a trait
//!
//! [`Steps`] is the seam `tests/upgrade.rs` drives, exactly as `deploy`'s seven steps are driven over shims: the
//! *order* is acceptance bullet 1's claim, and only some of it is a child a gate case can record (the `docker`
//! builds, the `systemctl` verbs). A recorder in process proves the rest — that a failed smoke test never reached the
//! drain, that an expired drain never wrote a backup, that a backup that could not be written never switched a
//! symlink — and the gate measures the whole thing end to end over a real drain socket and a stand-in systemd.
//!
//! # What it does not do
//!
//! No `rollback` verb beyond the automatic switch back; no pruning of old releases, no `doctor` reporting of
//! the resolved release path and no `version`-from-the-release-path; no provider call (the smoke run's own
//! limit, ADR-0081); and no database restore. One measured limit is recorded rather than fixed: the unit runs
//! `ExecStartPre=<exe> doctor --quick` through `current` too, and `doctor` refuses a database a *newer* release
//! wrote — so a rollback across a release that migrated the schema cannot start the old `serve`. That line
//! belongs to a later change and the refusal is ADR-0047's; this module's second poll measures it and the ADR records it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::backup;
use crate::builder;
use crate::config::Config;
use crate::db;
use crate::deploy::{self, Root};
use crate::drain;
use crate::http::health;
use crate::images;
use crate::progress;
use crate::release::{self, Release};
use crate::schema;
use crate::smoke;
use crate::version::VersionInfo;

/// How long the new version has to answer `/api/health` after a restart (spec item 10 step 5).
///
/// 20 seconds, the spec's number, and a parameter everywhere below it so a test can make it milliseconds — the same
/// argument `deploy::wait_for_health` makes about its bound.
pub const WINDOW: Duration = Duration::from_secs(20);

/// The mode the unit file is written with: `deploy`'s step 5 uses the same one, so the file a promotion writes and
/// the file a deploy would write differ in nothing but their `ExecStart`.
const UNIT_MODE: u32 = 0o644;

/// What the command line asked for.
#[derive(Debug, Clone)]
pub struct Options {
    /// The candidate: the file a promotion is about to make live.
    pub candidate: PathBuf,
    /// What recorded item 12's diff review, which is what lets the pipeline run at all. `None` is the refusal:
    /// there is no other path in this pipeline to the drain (ADR-0082 D2).
    pub review: Option<Review>,
    /// The verb the run is, and the prefix every line the pipeline prints carries: `upgrade` when an operator hands
    /// this machine a file, `update` when the run is a pass of the gate and then the pipeline ([`crate::pass`]).
    pub verb: &'static str,
    /// How long the drain may wait for the running turns (the flag's, default `10m`).
    pub wait: Duration,
    /// How long the new version has to answer after a restart: [`WINDOW`] for the command, and a parameter for the
    /// reason `deploy::Tools::health_wait` is one — a test that had to wait the spec's twenty seconds to prove the
    /// poll gives up would be a test nobody runs.
    pub window: Duration,
}

/// What recorded item 12's diff review, which is the requirement a promotion stands on.
///
/// Two because two things can be the record — a human's word and a machine's proof — and the pipeline's first
/// printed line says which one this run had. ADR-0110 D2 replaced the first with the second for a promotion the
/// browser starts; `--acknowledge-review` stays on the operator's own verb, because a human handing this machine a
/// file is a different act from a pass of the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Review {
    /// `--acknowledge-review`: a human read the diff and said so, and the flag is the record.
    Acknowledged,
    /// A green gate on the tree that built the candidate: the proof is the record (ADR-0110 D2).
    Verified,
}

impl Review {
    /// How the review line names this record: the one thing the two runs say differently.
    fn record(self) -> &'static str {
        match self {
            Self::Acknowledged => "acknowledged on the command line (--acknowledge-review)",
            Self::Verified => {
                "recorded by the gate's green run on the tree that built the candidate"
            }
        }
    }
}

/// The verb the hand path is, and the prefix every line of an `upgrade` run carries.
///
/// One spelling for the two places that name it — [`Real::new`]'s prefix and `commands::upgrade`'s
/// [`Options::verb`] — because a drain line saying `upgrade:` while the pipeline's said something else would be two
/// words for one run. `update` has a constant of its own ([`crate::pass::VERB`]) rather than sharing this one: the
/// two are two verbs, and a run the browser started must not have a line that names the operator's act instead.
pub const VERB: &str = "upgrade";

/// The candidate, as this command read it.
#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    bytes: Vec<u8>,
    digest: String,
    info: VersionInfo,
}

/// How one drain request ended, as the promotion reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainAnswer {
    /// This request started the drain and it finished: every running turn ended, every idle session was stopped.
    Drained,
    /// The wait ran out with a turn still running, and the machine stays draining.
    Expired,
    /// The machine was **already** draining when this request arrived: nothing here started it, and a promotion that
    /// waited out somebody else's drain would switch under a machine it did not quiet.
    Already,
    /// The request could not be made or could not be answered.
    Failed(String),
}

/// Everything a promotion asks the machine.
///
/// The production implementation is [`Real`]; the tests' is a recorder. Nothing here is a policy — the smoke run's
/// three codes, the drain's endings and the health answer's shape are the modules' own — so a test that drives this
/// trait measures *this* module's order and nothing else.
pub trait Steps {
    /// Run the host smoke test against the candidate, in the root the smoke run may remove.
    fn smoke(&self, candidate: &Path, root: &Path) -> smoke::Outcome;

    /// The daemon questions an image read and build ask.
    fn images(&self) -> &dyn images::Images;

    /// Ask the machine to drain, over its admin socket.
    fn drain(&self, admin_dir: &Path, wait: Duration) -> DrainAnswer;

    /// `systemctl --user restart <unit>`, whose exit status is the answer.
    fn restart(&self, tools: &deploy::Tools) -> Result<(), String>;

    /// `GET /api/health` once, on the control listener.
    fn health(&self, listen: SocketAddr) -> Result<health::Answer, String>;
}

/// The machine, for a real promotion: an in-process smoke run, the drain client, the daemon, and one child.
///
/// Two verbs ask through it — `upgrade` and `rollback` — and they differ in exactly two things, so both are fields
/// rather than a second implementation: the **prefix** its printed lines carry, and whether a drain **somebody else
/// started** is refused or joined (ADR-0083 D4).
#[derive(Debug, Clone)]
pub struct Real {
    config: Config,
    docker: crate::docker::Docker,
    prefix: &'static str,
    joins: bool,
}

impl Real {
    /// The promotion's machine: its lines say `upgrade:` and it refuses a drain this request did not start.
    pub fn new(config: &Config) -> Self {
        Self::named(config, VERB, false)
    }

    /// A promotion's machine whose printed lines carry `verb` — the run's own word for itself, as
    /// [`Options::verb`] is: `upgrade` for the hand path, `update` for a pass of the gate and then the pipeline
    /// ([`crate::pass`]). It refuses a drain this request did not start, exactly as [`Real::new`] does: `update` is a
    /// promotion, not a rollback.
    pub fn prefixed(config: &Config, verb: &'static str) -> Self {
        Self::named(config, verb, false)
    }

    /// The rollback verb's machine: its lines say `rollback:`, and it **joins** a drain already in progress.
    ///
    /// A promotion refuses on the drain's first line because switching under a machine it did not quiet would be the
    /// opposite of a refusal. A rollback must not: a machine left draining by a promotion that failed after its drain
    /// is exactly the machine that needs rolling back, and nothing but a restart clears a drain — which is what a
    /// rollback does. So the client keeps reading and `DrainAnswer::Already` cannot arise here; the verb would
    /// proceed if it did.
    pub fn joining(config: &Config) -> Self {
        Self::named(config, "rollback", true)
    }

    fn named(config: &Config, prefix: &'static str, joins: bool) -> Self {
        Self {
            config: config.clone(),
            docker: crate::docker::Docker::new(config.docker_host.clone()),
            prefix,
            joins,
        }
    }
}

impl Steps for Real {
    fn smoke(&self, candidate: &Path, root: &Path) -> smoke::Outcome {
        // In process, because ADR-0081 D1 made the smoke run's exit code a library answer for exactly this caller: a
        // second `holosmith` process reading the same config would be a second entry point to one pipeline.
        smoke::run(&self.config, candidate, root)
    }

    fn images(&self) -> &dyn images::Images {
        &self.docker
    }

    fn drain(&self, admin_dir: &Path, wait: Duration) -> DrainAnswer {
        let path = drain::socket_path(admin_dir);
        let prefix = self.prefix;
        let joins = self.joins;
        let mut already = false;
        let answer = crate::commands::drain::ask_with(
            &path,
            wait,
            crate::commands::drain::ANSWER_TIMEOUT,
            &mut |line| {
                println!("{prefix}: drain: {line}");
                // The one line this caller reads. A promotion stops on it: the machine was already draining, so this
                // request did not start it and waiting out somebody else's drain in order to switch under a machine
                // it did not quiet is the opposite of a refusal. A rollback joins the drain instead (ADR-0083 D4).
                if line == drain::ALREADY {
                    already = true;
                    joins
                } else {
                    true
                }
            },
        );
        if already && !joins {
            return DrainAnswer::Already;
        }
        match answer {
            Err(sentence) => DrainAnswer::Failed(sentence),
            Ok(exchange) => match exchange.ending {
                Some(drain::Ending::Drained) => DrainAnswer::Drained,
                Some(drain::Ending::Expired) => DrainAnswer::Expired,
                Some(drain::Ending::Refused) => DrainAnswer::Failed(format!(
                    "the machine refused the drain; its own line says why, and it is still draining if it managed to \
                     start: {}",
                    path.display()
                )),
                None => DrainAnswer::Failed(format!(
                    "{} closed the connection before saying how the drain ended; the machine is draining if it said \
                     so",
                    path.display()
                )),
            },
        }
    }

    fn restart(&self, tools: &deploy::Tools) -> Result<(), String> {
        // No escalation, and nothing to configure: the unit is the operator's own, written into their user manager's
        // directory beside their config (ADR-0088 D2), so the account running this command is the account that
        // manages it and `--user` is the whole of what says *which* manager. That is why there is no password to give,
        // no rule to install and no sentence below telling an operator to edit one — the failure a machine can have
        // here is a unit that will not start, so the sentence names where its own reason is written.
        let output = std::process::Command::new(&tools.systemctl)
            .args(["--user", "restart", deploy::UNIT_NAME])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| {
                format!(
                    "`{} --user restart {}` could not be run ({error}): a promotion restarts the unit the operator's \
                     user manager runs, and this restart never reached it — {}",
                    tools.systemctl.display(),
                    deploy::UNIT_NAME,
                    wont_start()
                )
            })?;
        if output.status.success() {
            return Ok(());
        }
        let said = String::from_utf8_lossy(&output.stderr);
        let line = said
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no output")
            .trim();
        Err(format!(
            "`{} --user restart {}` answered {} ({line}) — {}; a release cannot confirm itself live until it \
             starts",
            tools.systemctl.display(),
            deploy::UNIT_NAME,
            output.status.code().unwrap_or(-1),
            wont_start(),
        ))
    }

    fn health(&self, listen: SocketAddr) -> Result<health::Answer, String> {
        health::ask(listen)
    }
}

/// The whole promotion: what it was asked for, and the exit code its endings carry.
pub fn run(config: &Config, root: &Root, options: &Options, steps: &dyn Steps) -> ExitCode {
    let tools = deploy::Tools::default();
    match execute(config, root, options, steps, &tools) {
        Ok(()) => ExitCode::SUCCESS,
        Err(said) => {
            eprintln!("holosmith: {}: {said}", options.verb);
            ExitCode::FAILURE
        }
    }
}

/// [`run`] with the programs as a parameter, so a test can name its own `systemctl`.
pub fn execute(
    config: &Config,
    root: &Root,
    options: &Options,
    steps: &dyn Steps,
    tools: &deploy::Tools,
) -> Result<(), String> {
    // 0. The candidate, and the two facts that can refuse a promotion before anything is asked of the machine: it is
    //    not this machine's build, or its schema is older than the database's.
    let candidate = read_candidate(&options.candidate)?;
    let verb = options.verb;
    println!(
        "{verb}: candidate: {} (sha256 {}, {} ({}), schema {})",
        candidate.path.display(),
        &candidate.digest[..12],
        candidate.info.version,
        candidate.info.git_tag,
        candidate.info.schema_version
    );
    refuse_older_schema(
        config,
        &format!(
            "the candidate, {} (sha256 {})",
            candidate.path.display(),
            &candidate.digest[..12]
        ),
        candidate.info.schema_version,
        "build the release from the current tree (`scripts/build.sh`), or restore a database this candidate \
         understands",
    )?;

    // 1. The review gate. Item 12's review is the human gate, and what records it is the difference between the two
    //    runs: a flag on the operator's own verb, or a green gate on the tree that built the candidate (ADR-0110 D2).
    //    Either way the promotion has no other path to the drain.
    let Some(review) = options.review else {
        return Err(format!(
            "the review gate was not acknowledged: item 12's diff review is the real gate, and this command takes its \
             acknowledgement as `--acknowledge-review` (the candidate is {} at {}, sha256 {}) — nothing was drained, \
             backed up or switched",
            candidate.info.version, candidate.info.git_tag, candidate.digest
        ));
    };
    println!(
        "{verb}: review: {}, for {} sha256 {}: item 12's diff review is the gate, and this is the record of it",
        review.record(),
        candidate.info.git_tag,
        &candidate.digest[..12]
    );

    // 2. The smoke test, in process, while this machine still serves the version being replaced.
    let smoke_root = config.data_root.join("smoke");
    println!(
        "{verb}: smoke: {} --root {}",
        candidate.path.display(),
        smoke_root.display()
    );
    match steps.smoke(&candidate.path, &smoke_root) {
        smoke::Outcome::Passed => println!("{verb}: smoke: passed"),
        smoke::Outcome::Failed(sentence) => {
            return Err(format!(
                "the candidate failed its smoke test, so the promotion stopped before the drain: nothing was drained, \
                 nothing was backed up and nothing was switched.\n{sentence}"
            ));
        }
        smoke::Outcome::CouldNotRun(sentence) => {
            return Err(format!(
                "this machine cannot judge the candidate, so nothing was promoted: {sentence}"
            ));
        }
    }

    // 3. The images, for the candidate's version and from the candidate's bytes.
    //
    // The sink is the silent one, and the label names this pipeline's step rather than a deploy's: a promotion prints
    // a line per step of its own as it goes (`<verb>: images: …`), so a build's two lines would be a second spelling
    // of one progress stream — `install` is the verb whose silence the sink exists for (ADR-0092).
    let pending = images::outstanding(steps.images(), &candidate.info, &candidate.digest);
    match images::ensure(
        steps.images(),
        &candidate.path,
        &candidate.digest,
        &candidate.info,
        &pending,
        "images",
        None,
        &mut progress::Silent,
    ) {
        Ok(sentence) => println!("{verb}: images: {sentence}"),
        // The step's sentence, and a failed build is not this pipeline's exit-code question: a promotion that could not
        // fetch what it builds from is a promotion that did not run, and `upgrade`/`update` report it as one (ADR-0111).
        Err(error) => {
            return Err(format!(
                "the images the candidate's version needs were not built, so nothing was drained: {error}"
            ));
        }
    }

    // 4. The drain.
    match steps.drain(&crate::config::admin_dir(&config.data_root), options.wait) {
        DrainAnswer::Drained => println!("{verb}: drain: drained"),
        DrainAnswer::Already => {
            return Err(format!(
                "this machine is already draining (the drain answered `state: already draining`), so nothing was \
                 promoted: that drain is not this command's to wait out, and a drain is cleared by the restart it \
                 exists for (`systemctl --user restart {}`) rather than by finishing — nothing was backed up and \
                 nothing was switched",
                deploy::UNIT_NAME
            ));
        }
        DrainAnswer::Expired => {
            return Err(format!(
                "the drain's wait ran out with a turn still running, so nothing was promoted: this machine is still \
                 draining and keeps refusing new work, and `systemctl --user restart {}` is what clears a drain — \
                 nothing was backed up and nothing was switched",
                deploy::UNIT_NAME
            ));
        }
        DrainAnswer::Failed(sentence) => {
            return Err(format!(
                "the drain did not happen, so nothing was promoted: {sentence}"
            ));
        }
    }

    // 5. The database, before anything is switched. A backup that cannot be written is a promotion that does not
    //    happen — and from here on the machine is draining, which every later sentence says (one clause, one helper:
    //    `draining`).
    let backup = take_backup(config).map_err(draining)?;
    println!(
        "{verb}: backup: {} ({} kept)",
        backup.display(),
        backup::KEEP_SNAPSHOTS
    );

    // 6. The release, the unit and the switch. Each of these can refuse on a tree an operator has to look at, so
    //    each refusal carries the state the drain left the machine in.
    let release = Release::of(root);
    let previous = previous_release(&release, root, verb).map_err(draining)?;
    write_release(&release, &candidate, verb).map_err(draining)?;
    write_unit(root, config, &release, verb).map_err(draining)?;
    release.switch(&candidate.info.git_tag).map_err(draining)?;
    println!(
        "{verb}: current: -> releases/{} (was {})",
        candidate.info.git_tag,
        previous
            .tag
            .as_ref()
            .map(|tag| format!("releases/{tag}"))
            .unwrap_or_else(|| "no release".to_string())
    );

    // 7. The restart, and the poll that decides whether the candidate is live.
    serve_with(
        &candidate.info,
        config,
        root,
        steps,
        tools,
        options.window,
        &backup,
        &previous,
        verb,
    )
}

/// The candidate read from its bytes and its own `version --json`: the three checks that can refuse a file before the
/// machine is asked anything.
fn read_candidate(path: &Path) -> Result<Candidate, String> {
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "the candidate, {}, could not be read ({error}): a promotion makes the candidate the binary systemd \
             starts, so a candidate has to be a file this machine can run",
            path.display()
        )
    })?;
    let target = builder::default_target()?;
    if let Some(problem) = builder::artifact_problem(path, &bytes, target) {
        return Err(format!(
            "{problem}: a promotion makes the candidate the binary systemd starts, so it has to be this machine's \
             static build"
        ));
    }
    let digest = images::hex(&images::sha256(&bytes));
    // The digest file a build leaves beside the artifact. Checked, not trusted: a promotion's whole point
    // is the bytes that were reviewed, and the two files are how a build says which bytes those are.
    let digest_file = digest_path(path);
    let written = std::fs::read_to_string(&digest_file).map_err(|error| {
        format!(
            "{} could not be read ({error}): `scripts/build.sh` writes the artifact and its digest beside each \
             other, and a promotion reads both — a file with no digest is not a build this machine can promote",
            digest_file.display()
        )
    })?;
    let recorded = crate::remote::digest_of(&written).ok_or_else(|| {
        format!(
            "{} holds no sha256sum line, so the candidate's digest cannot be checked: `scripts/build.sh` writes \
             `<digest>  {}` there",
            digest_file.display(),
            release::BINARY
        )
    })?;
    if recorded != digest {
        return Err(format!(
            "{} is {} and its digest file says {recorded}: nothing was promoted, because the file is not the build \
             that was hashed (a changed artifact with an unchanged digest file is exactly what this check is for)",
            path.display(),
            digest
        ));
    }
    let info = ask_version(path)?;
    if let Some(problem) = release::tag_problem(&info.git_tag) {
        return Err(problem);
    }
    Ok(Candidate {
        path: path.to_path_buf(),
        bytes,
        digest,
        info,
    })
}

/// The digest file that belongs to `path`: `holosmith.sha256` beside `holosmith`.
fn digest_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".sha256");
    path.with_file_name(name)
}

/// One candidate's `version --json`, run as a temporary child.
///
/// A file that has not run cannot be asked, which is why `deploy --ssh` reads the ELF header instead — but here the
/// candidate *can* run (it is this machine's build), and `version --json` is a read: no daemon, no database, no
/// config. It is also the only honest source of the three facts a promotion needs — the version the health poll looks
/// for, the git tag the release directory is named after, and the schema the candidate understands.
pub fn ask_version(binary: &Path) -> Result<VersionInfo, String> {
    let output = std::process::Command::new(binary)
        .args(["version", "--json"])
        .output()
        .map_err(|error| {
            format!(
                "{} could not be run for its `version --json` ({error}): it is not executable, or it is not a \
                 program this machine can run",
                binary.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "{} answered {} to `version --json` ({}): a promotion reads the candidate's version, tag and schema from \
             the candidate itself",
            binary.display(),
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice::<VersionInfo>(&output.stdout).map_err(|error| {
        format!(
            "{} answered `version --json` with something this build cannot read ({error}): {}",
            binary.display(),
            String::from_utf8_lossy(&output.stdout).trim()
        )
    })
}

/// Acceptance bullet 5's second refusal: a binary whose schema is older than the database's.
///
/// Read **without migrating** (`db::recorded_without_migrating`, ADR-0047): a promotion must not open the database
/// read-write to ask a question, because that open applies the migrations and would change the very file the backup
/// two steps later is about to take — and a rollback is deciding whether the release it is about to switch to could
/// read this database at all.
///
/// One function with two callers and one sentence each, differing only in `subject` — the candidate a promotion is
/// about to make live, or the release a rollback is about to switch back to. Both are the same measurement (`schema`
/// against what the database records), which is why it is not spelled twice.
///
/// `remedy` is the other half of acceptance bullet 2's "with a clear explanation", and it is the one part the two
/// verbs cannot share: what an operator should do next when it is a *candidate* that is too old (build the current
/// tree, or restore a database it understands) is not what they should do when it is a *release in the tree*
/// (`--to` another one).
pub(crate) fn refuse_older_schema(
    config: &Config,
    subject: &str,
    schema: u32,
    remedy: &str,
) -> Result<(), String> {
    let path = config.data_root.join(db::DATABASE_FILE);
    let recorded = db::recorded_without_migrating(&path).map_err(|error| {
        format!(
            "the database at {} could not be read: {error}",
            path.display()
        )
    })?;
    let Some(recorded) = recorded else {
        // No database: a machine that has never served, whose drain will refuse a few steps later. There is nothing
        // to be older than.
        return Ok(());
    };
    if schema::verdict(recorded, schema) != schema::Verdict::Newer {
        return Ok(());
    }
    Err(format!(
        "{subject} reports schema {schema} and the database at {} records {recorded}: {subject} is older than the \
         database, and migrations are forward-only, so it could not read what a newer release wrote. Nothing was \
         changed — {remedy}. The unit runs `doctor --quick` before `ExecStart`, so this release would refuse to start \
         on this database even if `current` named it.\n{}",
        path.display(),
        schema::Verdict::Newer.sentence(recorded, schema)
    ))
}

/// `VACUUM INTO backups/app-<ts>.db`, five kept.
fn take_backup(config: &Config) -> Result<PathBuf, String> {
    let path = config.data_root.join(db::DATABASE_FILE);
    let reader = db::Reader::open(&path)
        .map_err(|error| format!("the database at {} could not be opened: {error}", path.display()))?
        .ok_or_else(|| {
            format!(
                "there is no database at {} to back up: a promotion backs the machine's database up before it \
                 switches a release, so this is a machine that has never served",
                path.display()
            )
        })?;
    let snapshots = backup::Snapshots::new(&config.data_root);
    snapshots
        .take(crate::clock::unix_now(), &|destination| {
            reader
                .snapshot(destination)
                .map_err(|error| error.to_string())
        })
        .map_err(|error| format!("the database backup failed, so nothing was switched: {error}"))
}

/// The clause every refusal **after** a successful drain owes the operator.
///
/// The drain is the point of no return for the machine's *state*, not for the release: a promotion that fails after
/// it has left the process draining (new creates and prompts answer `503 draining`, reads keep answering), and the
/// only thing that clears a drain is the restart it exists for. Naming that in each sentence is what
/// `AGENTS.md`'s "every refusal leaves the machine as it found it — except after the drain" means, and it is one
/// function so the five call sites cannot drift from it.
pub(crate) fn draining(said: String) -> String {
    format!(
        "{said} — this machine is draining until it is restarted (`systemctl --user restart {}`)",
        deploy::UNIT_NAME
    )
}

/// Where a unit that will not start says so itself: the user manager's verdict, and the journal behind it.
///
/// Four sentences carry this clause — both refusals a restart comes back with, and the two endings that report a
/// release that never answered — and all four are answering one question: the switch happened, the service is not
/// there, so where is the reason? It is one function for the reason [`draining`] is one: `--user` is the half that
/// matters, and a sentence that asks the system manager names a unit it does not have, so the operator who copies it
/// is answered "Unit holosmith.service could not be found" rather than with the failing start (ADR-0088 D2).
///
/// What it carries is the two commands and what each one answers, not the sentence they sit in: an ending says the
/// switch is unconfirmed and points at the reason, and a refusal reports what the manager answered, so each caller
/// keeps its own frame.
pub(crate) fn wont_start() -> String {
    format!(
        "`systemctl --user status {}` says what the user manager makes of the unit and `journalctl --user -u {}` \
         says what it printed",
        deploy::UNIT_NAME,
        deploy::UNIT_NAME
    )
}

/// What was live before the switch: the tag `current` names, and the binary to read its version back from.
#[derive(Debug, Clone)]
struct Previous {
    /// The tag, when `current` named one of this tree's own releases.
    tag: Option<String>,
    /// The link target exactly as it was written, so a rollback restores it verbatim.
    link: PathBuf,
}

/// Read what `current` points at, or seed it from the binary the unit runs.
///
/// A machine that has never promoted has no `current`, and the previous release is the one systemd is running: this
/// reads the unit's `ExecStart=`, copies that binary into `releases/<its tag>/` and points `current` at it **before**
/// the candidate's release is written — so the rollback target exists before the switch that may need it.
fn previous_release(release: &Release, root: &Root, verb: &str) -> Result<Previous, String> {
    if let Some(link) = release.link() {
        let tag = release.tag_of(&link).ok_or_else(|| {
            format!(
                "{} names {} at {}, which is not one of this tree's releases: a promotion switches `current` between \
                 `releases/<tag>` directories and has to be able to switch it back — point the link at a release, or \
                 remove it and let this command seed one from the unit",
                release.current().display(),
                link.display(),
                release.root().display()
            )
        })?;
        if !release.binary(&tag).is_file() {
            return Err(format!(
                "`current` names releases/{tag}, and {} is not there: a rollback would have nowhere to go, so nothing \
                 was switched",
                release.binary(&tag).display()
            ));
        }
        return Ok(Previous {
            tag: Some(tag),
            link,
        });
    }
    // No `current`: the unit's `ExecStart` is what is live.
    let unit_path = root.unit();
    let text = std::fs::read_to_string(&unit_path).map_err(|error| {
        format!(
            "{} could not be read ({error}): a promotion switches the `current` symlink the unit runs and restarts \
             it, so this machine has to have been deployed (`holosmith server deploy`), and the unit is where the \
             binary it is running is named",
            unit_path.display()
        )
    })?;
    let exe = release::unit_executable(&text).ok_or_else(|| {
        format!(
            "{} names no `ExecStart=`: a promotion needs the binary this machine is running, because that is the \
             release a rollback goes back to",
            unit_path.display()
        )
    })?;
    let bytes = std::fs::read(&exe).map_err(|error| {
        format!(
            "the unit's `ExecStart` names {}, which could not be read ({error}): that binary is the release a \
             rollback goes back to, and `holosmith server deploy` is the command that writes a unit naming a release \
             this machine holds",
            exe.display()
        )
    })?;
    let info = ask_version(&exe)?;
    if let Some(problem) = release::tag_problem(&info.git_tag) {
        return Err(format!(
            "{problem}: the binary {}'s tag names the release it is seeded as",
            exe.display()
        ));
    }
    let digest = images::hex(&images::sha256(&bytes));
    release.write(&info.git_tag, &bytes, &digest)?;
    release.switch(&info.git_tag)?;
    println!(
        "{verb}: release: seeded releases/{} from {} ({} ({}), the binary the unit runs)",
        info.git_tag,
        exe.display(),
        info.version,
        info.git_tag
    );
    let link = Path::new(release::RELEASES).join(&info.git_tag);
    Ok(Previous {
        tag: Some(info.git_tag),
        link,
    })
}

/// Write the candidate's release directory: its bytes and its digest, the two files a release directory holds.
fn write_release(release: &Release, candidate: &Candidate, verb: &str) -> Result<(), String> {
    if release.binary(&candidate.info.git_tag) == candidate.path {
        // The candidate is already the release: a promotion run from `releases/<tag>/holosmith` itself. Nothing to
        // copy, and the switch below is all that is left.
        println!(
            "{verb}: release: {} is the candidate (sha256 {})",
            candidate.path.display(),
            &candidate.digest[..12]
        );
        return Ok(());
    }
    release.write(&candidate.info.git_tag, &candidate.bytes, &candidate.digest)?;
    println!(
        "{verb}: release: {} (sha256 {})",
        release.binary(&candidate.info.git_tag).display(),
        &candidate.digest[..12]
    );
    Ok(())
}

/// Make the unit's `ExecStart` go through `current`, so the restart starts the switched release.
///
/// The promotion does not own the unit's content — [`deploy::unit_text`] does, and `deploy`'s step 5 renders the same
/// bytes from the same function — it owns only the *path* that function is given, which is the one fact a promotion
/// changes. A unit that already names `current/holosmith` is left byte-identical, so a second promotion writes
/// nothing and a later `server deploy` finds the file it would have written.
pub(crate) fn write_unit(
    root: &Root,
    config: &Config,
    release: &Release,
    prefix: &str,
) -> Result<(), String> {
    let path = root.unit();
    let exe = release.current().join(release::BINARY);
    let bytes = deploy::unit_text(&exe, config.role, root);
    if std::fs::read(&path).is_ok_and(|existing| existing == bytes.as_bytes()) {
        return Ok(());
    }
    let directory = path.parent().unwrap_or(Path::new(deploy::UNIT_DIR));
    std::fs::create_dir_all(directory).map_err(|error| {
        format!(
            "{} could not be created ({error}): the unit is what systemd reads, and a release verb writes it so that it \
             it runs through `current`",
            directory.display()
        )
    })?;
    std::fs::write(&path, bytes.as_bytes()).map_err(|error| {
        format!(
            "{} could not be written ({error}): the unit is what systemd reads, and a release verb writes it so that it \
             it runs through `current`",
            path.display()
        )
    })?;
    std::fs::set_permissions(
        &path,
        std::os::unix::fs::PermissionsExt::from_mode(UNIT_MODE),
    )
    .map_err(|error| format!("{} could not be made read-only ({error})", path.display()))?;
    println!(
        "{prefix}: unit: {} (ExecStart through `current`)",
        path.display()
    );
    Ok(())
}

/// `systemctl --user restart`, then the poll; then, if the candidate did not answer, the switch back.
#[allow(clippy::too_many_arguments)]
fn serve_with(
    candidate: &VersionInfo,
    config: &Config,
    root: &Root,
    steps: &dyn Steps,
    tools: &deploy::Tools,
    window: Duration,
    backup: &Path,
    previous: &Previous,
    verb: &str,
) -> Result<(), String> {
    // The clock starts **before** the restart is asked for: the poll accepts only an answer from a process younger
    // than this instant, which is what separates the candidate from the version it replaced.
    let attempt = restart_and_poll(
        config,
        steps,
        tools,
        Some(candidate.version.as_str()),
        window,
        verb,
    );
    if let Some(answer) = attempt.answered {
        println!(
            "{verb}: health: {} answered {:.1}s after the restart",
            answer.version,
            attempt.started.elapsed().as_secs_f32()
        );
        println!(
            "{verb}: live: {} ({}), backup {}",
            candidate.version,
            candidate.git_tag,
            backup.display()
        );
        // Step 8, and the only place a prune runs: the candidate is live, so the tree may shed what it no longer
        // needs. A promotion that failed before here, or whose candidate never answered, removes nothing — that tree
        // is what an operator reads while diagnosing it, and the rollback target is in it.
        prune(&Release::of(root), config.keep_releases, verb);
        return Ok(());
    }
    // The rollback: the same switch in reverse, the same restart, and the same poll -- and **no database restore**,
    // because expand/contract is what makes switching back safe (ADR-0047). A restore would delete the work of every
    // session since the backup.
    rollback(config, root, steps, tools, window, backup, previous, verb)
}

/// Step 8: keep the newest `keep_releases`, and never the live release or the one behind it.
///
/// A **warning**, never a failed promotion: the product of this command is a candidate that is serving, and a
/// directory it could not delete leaves a tree with one release too many — a fact to print, not a reason to tell an
/// operator that a healthy promotion failed (which would read as "roll back"). The sentence says what was removed
/// before it stopped, because a half-done prune is the state a second run has to be able to read.
fn prune(release: &Release, keep: u32, verb: &str) {
    let held = release.history().len();
    match release.prune(keep) {
        Ok(done) => {
            let removed = if done.removed.is_empty() {
                "nothing".to_string()
            } else {
                done.removed
                    .iter()
                    .map(|tag| format!("releases/{tag}"))
                    .collect::<Vec<String>>()
                    .join(", ")
            };
            println!(
                "{verb}: prune: kept {} of {held} (keep_releases = {keep}); removed: {removed}",
                done.kept.len()
            );
        }
        Err(said) => eprintln!("holosmith: warning: {verb}: prune: {said}"),
    }
}

/// Put the previous link target back, restart, and say what is live now -- the promotion's failure ending.
#[allow(clippy::too_many_arguments)]
fn rollback(
    config: &Config,
    root: &Root,
    steps: &dyn Steps,
    tools: &deploy::Tools,
    window: Duration,
    backup: &Path,
    previous: &Previous,
    verb: &str,
) -> Result<(), String> {
    let release = Release::of(root);
    release.restore(&previous.link).map_err(|error| {
        format!(
            "the candidate did not become live and `current` could not be put back to {}: {error}. This machine is \
             draining until it is restarted, and {} is what it was switched away from -- restore it by hand, or \
             restore the backup at {}",
            previous.link.display(),
            previous.link.display(),
            backup.display()
        )
    })?;
    println!("{verb}: rollback: current -> {}", previous.link.display());
    // The previous release's own version, for the poll and for the last line: read from the binary that is now
    // `current`, which is the one a rollback made live.
    let version = previous
        .tag
        .as_ref()
        .and_then(|tag| ask_version(&release.binary(tag)).ok());
    let switch_back = format!("{verb}: rollback");
    let attempt = restart_and_poll(
        config,
        steps,
        tools,
        version.as_ref().map(|info| info.version.as_str()),
        window,
        // The automatic switch-back's own lines sit under `<verb>: rollback:` — it is the promotion's rollback, and
        // the `rollback` verb prints its identical sequence under `rollback:`.
        &switch_back,
    );
    let answered = attempt.answered;
    let live = match &version {
        Some(info) => format!("{}, {}", info.version, info.git_tag),
        None => "its own binary answers no `version --json`".to_string(),
    };
    if let Some(answer) = answered {
        println!(
            "{verb}: rollback: {} answered {:.1}s after the restart",
            answer.version,
            attempt.started.elapsed().as_secs_f32()
        );
        // The release the link names **first**, because that is the thing an operator can see in `releases/`: a
        // release directory's name is a promotion's own choice (`releases/<tag>`), and the binary inside reports the
        // tag it was built from — which a hand-seeded directory need not share.
        println!(
            "{verb}: live: {} ({live}) (the candidate was not made live), backup {}",
            previous.link.display(),
            backup.display()
        );
        return Err(format!(
            "the candidate did not answer `/api/health` as a process younger than the restart, so it was rolled \
             back: {} is live again and answered as {live}, and the backup is {}. The database was not restored -- \
             the migration rule (expand/contract) is what makes switching back safe",
            previous.link.display(),
            backup.display()
        ));
    }
    println!("{verb}: rollback: {live} did not answer either");
    println!(
        "{verb}: live: {} ({live}) could not be confirmed, backup {}",
        previous.link.display(),
        backup.display()
    );
    Err(format!(
        "the candidate did not answer, and neither did the release it was switched back to ({} as {live}): nothing is \
         confirmed live. {} -- if the candidate migrated the database, this release's `doctor --quick` refuses it \
         before `ExecStart` (ADR-0047), and the backup at {backup} is the state before the promotion",
        previous.link.display(),
        wont_start(),
        backup = backup.display()
    ))
}

/// One attempt at making a switch live: the clock, the restart, and the answer that counted.
///
/// The two verbs that switch `current` — a promotion's candidate and a rollback's target — ask the machine the same
/// two questions in the same order, and only the prose of their endings differs. This type is that order, so neither
/// can put the poll before the restart or forget that the clock starts first.
pub(crate) struct Attempt {
    /// When the clock started: **before** the restart was asked for, which is the fact [`poll`] compares against.
    pub started: Instant,
    /// What `systemctl --user restart` answered.
    pub restarted: Result<(), String>,
    /// The `/api/health` answer that counted, when one did.
    pub answered: Option<health::Answer>,
}

/// `systemctl --user restart`, then the poll for a process younger than it.
///
/// `version` is `None` only for a release whose own binary cannot answer `version --json`: there is nothing to look
/// for then, so the restart is still run and the poll is not. `prefix` is what the printed lines say — `upgrade`,
/// `upgrade: rollback` or `rollback` — because three runs of this sequence are three different things to read.
pub(crate) fn restart_and_poll(
    config: &Config,
    steps: &dyn Steps,
    tools: &deploy::Tools,
    version: Option<&str>,
    window: Duration,
    prefix: &str,
) -> Attempt {
    let started = Instant::now();
    let restarted = steps.restart(tools);
    println!(
        "{prefix}: restart: {} --user restart {}",
        tools.systemctl.display(),
        deploy::UNIT_NAME
    );
    let answered = match (&restarted, version) {
        (Ok(()), Some(want)) => poll(steps, config.control_listen, want, started, window, prefix),
        (Err(sentence), _) => {
            println!("{prefix}: restart: {sentence}");
            None
        }
        (Ok(()), None) => None,
    };
    Attempt {
        started,
        restarted,
        answered,
    }
}

/// Poll `/api/health` until an answer is **this** process, or `window` elapses.
///
/// Three facts, and the third is what makes the poll falsifiable: every binary this repo builds reports the same
/// `version`, so the version alone would be satisfied by the *old* process still answering during the restart. A
/// process that says it started **after** the restart is the evidence, and `uptime_s` is the payload's own answer to
/// "how long have you been running" — it does not say *which* binary started, which is why a promotion between two
/// builds of one crate version is recorded as "a process started after the restart answered" and nothing more.
///
/// The giving-up sentence is printed here rather than at the callers, so that the two verbs' promises about what a
/// failed poll sounds like cannot drift apart.
fn poll(
    steps: &dyn Steps,
    listen: SocketAddr,
    want: &str,
    started: Instant,
    window: Duration,
    prefix: &str,
) -> Option<health::Answer> {
    loop {
        let last = match steps.health(listen) {
            Ok(answer) => {
                // A process that started after the restart: `uptime_s` is the payload's own answer to "how long have
                // you been running", and the second of slack covers a clock that ticks between the restart and the
                // first answer.
                let fresh = answer.uptime_s <= started.elapsed().as_secs().saturating_add(1);
                if answer.name == health::NAME && answer.version == want && fresh {
                    return Some(answer);
                }
                format!(
                    "the last answer was {} with uptime {}s",
                    answer.version, answer.uptime_s
                )
            }
            Err(sentence) => sentence,
        };
        let elapsed = started.elapsed();
        if elapsed >= window {
            if !last.is_empty() {
                println!("{prefix}: health: {last}");
            }
            println!(
                "{prefix}: health: no answer with {want} in {}s",
                window.as_secs()
            );
            return None;
        }
        std::thread::sleep(deploy::HEALTH_POLL.min(window - elapsed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A fake admin socket that says `state: already draining` and then how the drain ended, and closes.
    ///
    /// That is the server's real shape when a second request arrives during somebody else's drain: `Drain::run`
    /// prints the line, runs on to the ending, and answers it (`crate::drain`).
    fn draining(label: &str) -> (PathBuf, std::thread::JoinHandle<()>) {
        let dir = std::env::temp_dir().join(format!(
            "holosmith-promote-drain-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the socket's directory is creatable");
        let path = drain::socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("the fake socket binds");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a client connects");
            let mut request = [0_u8; 64];
            let _ = std::io::Read::read(&mut stream, &mut request);
            for line in [drain::ALREADY, "result: drained"] {
                if std::io::Write::write_all(&mut stream, format!("{line}\n").as_bytes()).is_err() {
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        });
        (dir, handle)
    }

    /// The two verbs' one difference, measured where it is made rather than at the verbs that read it.
    ///
    /// A promotion stops on the server's first line and refuses (`DrainAnswer::Already`); a rollback joins the drain
    /// and takes whatever ending it reaches (`Drained` here). Flipping either constructor's policy reddens this.
    #[test]
    fn a_promotion_refuses_a_drain_somebody_else_started_and_a_rollback_joins_it() {
        let config = Config::default();
        let (dir, server) = draining("promotion");
        assert_eq!(
            Real::new(&config).drain(&dir, std::time::Duration::from_secs(5)),
            DrainAnswer::Already,
            "a promotion must refuse rather than wait out a drain it did not start"
        );
        server.join().expect("the fake server finishes");

        let (dir, server) = draining("rollback");
        assert_eq!(
            Real::joining(&config).drain(&dir, std::time::Duration::from_secs(5)),
            DrainAnswer::Drained,
            "a rollback must join the drain and take its ending"
        );
        server.join().expect("the fake server finishes");
    }

    /// A `systemctl` that records its argv and exits `status`, as an executable script.
    ///
    /// A shim rather than a recorder in process, because the question is what the **child** is given: `--user` is
    /// argv, and the one bug it can have is being spelled nowhere but in a printed line.
    fn systemctl_shim(label: &str, status: i32) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "holosmith-promote-systemctl-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the shim's directory is creatable");
        let log = dir.join("argv");
        let program = dir.join("systemctl");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n{{ printf '%s\\n' \"$@\"; }} > \"{}\"\nexit {status}\n",
                log.display()
            ),
        )
        .expect("the shim is writable");
        std::fs::set_permissions(
            &program,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("the shim is executable");
        (program, log)
    }

    /// The restart asks the operator's **own** user manager, and a manager that refuses says what to read.
    ///
    /// The unit is written beside the operator's config and managed by their `systemctl --user` (ADR-0088 D2), so the
    /// restart is unprivileged and there is no rule to install — which makes the two facts worth measuring exactly
    /// the argv the child receives and the sentences a failure carries. An escalation creeping back in would have to
    /// redden one of them.
    #[test]
    fn a_restart_asks_the_operators_user_manager_and_names_what_to_read_when_it_fails() {
        let config = Config::default();

        let (program, log) = systemctl_shim("accepted", 0);
        let tools = deploy::Tools {
            systemctl: program,
            ..deploy::Tools::default()
        };
        Real::new(&config)
            .restart(&tools)
            .expect("a restart the user manager accepted is a success");
        let argv = std::fs::read_to_string(&log).expect("the shim recorded its argv");
        assert_eq!(
            argv.lines().collect::<Vec<&str>>(),
            ["--user", "restart", deploy::UNIT_NAME],
            "the restart did not ask the user manager for the unit: {argv:?}"
        );

        let (program, _) = systemctl_shim("refused", 1);
        let tools = deploy::Tools {
            systemctl: program,
            ..deploy::Tools::default()
        };
        let said = Real::new(&config)
            .restart(&tools)
            .expect_err("a user manager that refused is a failed restart");
        assert!(
            said.contains("--user status holosmith.service"),
            "the failure does not say where the manager's own reason is: {said}"
        );
        assert!(
            said.contains("journalctl --user -u holosmith.service"),
            "the failure does not say where the unit's output is: {said}"
        );
        assert!(
            !said.contains("sudo"),
            "the failure names an escalation the operator's own unit does not need: {said}"
        );
    }
}
