//! `doctor`: whether this machine is fit to run sessions (HS-045, spec item 10 §1 and §7, V2).
//!
//! The command is two depths of **one list**, because that is how the spec uses it: `doctor --quick` is what
//! `server deploy` step 1 runs and what the unit file runs as `ExecStartPre` (item 10 §5), and `doctor --full`
//! is item 10 §7 — run *after* the deploy, once the machine is already serving — which adds the egress
//! self-test (whose bare probe **is** the network-off probe, ADR-0044/ADR-0046), the two proxies' state and the
//! agent image's pi version.
//!
//! Three rules hold the design together:
//!
//! - **The preflight's three checks are reused, not re-implemented.** They are [`crate::preflight`]'s report,
//!   printed here; `serve` refuses on the same sentences (ADR-0028).
//! - **Only a failure changes the exit code.** An [`Outcome::Unknown`] is "this check did not produce a verdict
//!   about whether `serve` may run here", and its sentence says what was missing and what does need it. That is
//!   ADR-0044's rule for the self-test, applied to a machine with no systemd and no Tailscale daemon — where
//!   `serve` by hand is a supported way to run (ADR-0054).
//! - **`doctor` starts nothing that outlives it.** The self-test removes its own network and probe container,
//!   the image question is a `docker run --rm --network none`, and the proxies are *read*, not started. There is
//!   no unit file to write: `ExecStartPre=holosmith doctor --quick` is HS-049's line, and this story's contract
//!   is the exit code and one actionable line per failure.
//!
//! The one check that is a measurement rather than a question is [`merge`] — V2's "`git merge-tree --write-tree`
//! exits 0 for a clean merge and 1 with paths for a conflict" — and it runs on a throwaway repository under
//! `std::env::temp_dir()`, which is the only way to answer it without owning a project.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::agent;
use crate::config::{Config, Role};
use crate::container::{PROXY_ALIAS, PROXY_NAME};
use crate::db;
use crate::docker::Docker;
use crate::key::{KEY_PROXY_ALIAS, KEY_PROXY_NAME};
use crate::preflight::{self, Check, Finding, Outcome};
use crate::schema::{self, Verdict};
use crate::selftest;

/// Where systemd says it is running: a directory, not a process table, because that is the fact a script can
/// read without a subprocess (and `/proc/1/comm` can be `bwrap` or `tini` for reasons that are not "no systemd").
pub const SYSTEMD_DIR: &str = "/run/systemd/system";

/// How long one connection to a listener gets before it is "not answering".
///
/// The same bound [`crate::preflight`] gives the daemon, for the same reason: a listener that accepts and says
/// nothing must cost a check's deadline, not the command.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The three files the merge trial writes, so the cleanup is a directory and not a list.
const MERGE_FILE: &str = "base.txt";
const MERGE_ADDED: &str = "added.txt";

/// How much of the report the operator asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// The list `serve` refuses on and `ExecStartPre` runs: no container, no daemon beyond `_ping`.
    Quick,
    /// Everything `Quick` has, plus the self-test, the proxies and the image's pi version.
    Full,
}

impl Depth {
    /// The word the report's header prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Full => "full",
        }
    }
}

impl std::fmt::Display for Depth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The external programs and paths the checks read, as a seam.
///
/// [`preflight::Tools`] is embedded rather than copied, because the preflight's three checks are the same three
/// `serve` runs: one list of shims, one behaviour. The two additions are doctor's own question — `tailscale`
/// is a program, and systemd is a directory — and both are a `PathBuf` for `preflight::Tools`'s reason: a test
/// points them at what it wrote, because this crate denies `unsafe` and so cannot set `PATH`.
#[derive(Debug, Clone)]
pub struct Tools {
    pub preflight: preflight::Tools,
    pub tailscale: PathBuf,
    pub systemd: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            preflight: preflight::Tools::default(),
            tailscale: PathBuf::from("tailscale"),
            systemd: PathBuf::from(SYSTEMD_DIR),
        }
    }
}

/// The machine questions `--full` asks that need a daemon, as a seam — the [`preflight::Tools`] idea applied to
/// the three things a test cannot own.
///
/// [`Docker`] is the only production implementation. The self-test joins it rather than being re-run: it already
/// takes a `Runner`, and `Docker` implements that too, so `doctor` calls the shipped entry point.
pub trait Machine {
    /// Whether a container by this name is running (`Docker::running`).
    fn running(&self, name: &str) -> bool;

    /// The pi version an image runs (`agent::version_of`): the image's own `pi --version` in a `--rm` container
    /// with no network.
    fn image_pi(&self, image: &str) -> Result<String, String>;

    /// The network self-test, whose bare probe is the network-off probe (ADR-0044/ADR-0046).
    fn selftest(&self, image: &str) -> selftest::Outcome;
}

impl Machine for Docker {
    fn running(&self, name: &str) -> bool {
        Docker::running(self, name)
    }

    fn image_pi(&self, image: &str) -> Result<String, String> {
        agent::version_of(self, image)
    }

    fn selftest(&self, image: &str) -> selftest::Outcome {
        selftest::run(self, image)
    }
}

/// Every check's answer, in the order `doctor` prints them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub depth: Depth,
    pub findings: Vec<Finding>,
}

impl Report {
    /// Whether anything failed, which is exactly what the exit code says.
    pub fn failed(&self) -> bool {
        self.findings.iter().any(Finding::is_failure)
    }

    /// The failures, in order — the "one actionable line per failure" half of the acceptance, for a caller that
    /// wants the sentences without the passes.
    pub fn failures(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|finding| finding.is_failure())
    }

    /// The whole report as the operator reads it: the depth, one line per check, and the tally.
    pub fn render(&self) -> String {
        let mut text = format!("doctor: {}\n", self.depth);
        for finding in &self.findings {
            // A `String` cannot fail to be written to, which is why this is not a `?`.
            let _ = writeln!(text, "{finding}");
        }
        let count = |outcome: Outcome| {
            self.findings
                .iter()
                .filter(|finding| finding.outcome == outcome)
                .count()
        };
        let _ = writeln!(
            text,
            "doctor: {} findings: {} pass, {} fail, {} unknown",
            self.findings.len(),
            count(Outcome::Pass),
            count(Outcome::Fail),
            count(Outcome::Unknown)
        );
        text
    }
}

/// Run the checks `depth` names, in the order the report prints them.
///
/// The order is the operator's: the machine's init, then the three `serve` refuses on (with V2's merge trial
/// beside git), then disk, Tailscale, the ports and the database; `--full` appends the container questions. A
/// **hub** is not asked the Docker or git questions at all — item 10 §6's `hub deploy` runs step 1 "without the
/// Docker and git checks", because a hub runs no sessions — while disk, Tailscale, ports and the database still
/// belong to it.
pub fn run(config: &Config, depth: Depth, tools: &Tools, machine: &dyn Machine) -> Report {
    let mut findings = vec![systemd(&tools.systemd)];
    let mut docker_ok = false;
    for finding in preflight::report_with(config, &tools.preflight) {
        match finding.check {
            Check::Docker | Check::Git if config.role == Role::Hub => continue,
            Check::Docker => {
                docker_ok = finding.outcome == Outcome::Pass;
                findings.push(finding);
            }
            Check::Git => {
                let git_ok = finding.outcome == Outcome::Pass;
                findings.push(finding);
                findings.push(merge(&tools.preflight.git, git_ok));
            }
            _ => findings.push(finding),
        }
    }
    findings.push(tailscale(&tools.tailscale));
    // Item 10's two moments: `--quick` runs before a start, when a free port is the pass, and `--full` runs
    // after one, when the control port is held by this machine's own `serve` — so "serving" is what a port in
    // use means there, and it is measured once, by the health endpoint, for both ports.
    let serving = (depth == Depth::Full).then(|| health_answers(config.control_listen));
    findings.extend(ports(config, serving));
    findings.push(database(config));
    if depth == Depth::Full {
        let image = config.agent_image.as_str();
        findings.push(network(machine, image, docker_ok));
        findings.push(proxy(machine, PROXY_NAME, PROXY_ALIAS, docker_ok));
        findings.push(proxy(machine, KEY_PROXY_NAME, KEY_PROXY_ALIAS, docker_ok));
        findings.push(image_check(machine, image, docker_ok));
    }
    Report { depth, findings }
}

/// Whether this machine has systemd, where "not there" is a fact rather than a failure.
///
/// `/run/systemd/system` exists exactly while systemd is the init, and its absence is the supported way to run
/// `serve` by hand (docs/build.md says so). A `server deploy` (HS-049) is where systemd becomes a prerequisite,
/// and that step is where the refusal belongs — refusing here would make `doctor` unrunnable on the machine that
/// runs no systemd and serves perfectly well.
fn systemd(dir: &Path) -> Finding {
    if dir.is_dir() {
        return Finding::pass(
            Check::Systemd,
            format!(
                "{} is there, so systemd is this machine's init",
                dir.display()
            ),
        );
    }
    Finding::unknown(
        Check::Systemd,
        format!(
            "{} is not there, so this machine has no systemd: `serve` by hand is supported and needs none, and \
             `server deploy` (HS-049) is where systemd becomes a prerequisite",
            dir.display()
        ),
    )
}

/// Whether Tailscale is installed, its daemon answers, and the node is logged in.
///
/// Three outcomes and a real failing arm: a daemon that answers "logged out" is a machine an operator can fix
/// with one command, and it is measured rather than inferred. A daemon that does not answer is *unknown*, not a
/// failure — a machine reached only on the LAN does not need Tailscale (ADR-0054).
fn tailscale(tailscale: &Path) -> Finding {
    let output = match Command::new(tailscale).args(["status", "--json"]).output() {
        Ok(output) => output,
        Err(error) => {
            return Finding::unknown(
                Check::Tailscale,
                format!(
                    "the tailscale CLI is not runnable ({error}): a machine reached only on the LAN does not need \
                     it, and a hub's phone access and `tailscale serve` do (spec items 10 §6 and 11)"
                ),
            );
        }
    };
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let said = if said.is_empty() {
            format!("exit {}", output.status.code().unwrap_or(-1))
        } else {
            said
        };
        return Finding::unknown(
            Check::Tailscale,
            format!(
                "the tailscale daemon did not answer ({said}): `serve` by hand does not need it, and a hub's \
                 phone access does — start it with `sudo systemctl start tailscaled` when this machine is to be \
                 reached off-LAN"
            ),
        );
    }
    match backend_state(&String::from_utf8_lossy(&output.stdout)).as_deref() {
        Some("Running") => Finding::pass(
            Check::Tailscale,
            "`tailscale status` reports a logged-in node",
        ),
        Some(state) => Finding::fail(
            Check::Tailscale,
            format!(
                "Tailscale is {state} and this node is not logged in: run `tailscale up` on this machine, which is \
                 what serves the control origin (spec item 10 §6) and lets a phone reach it (item 11)"
            ),
        ),
        None => Finding::unknown(
            Check::Tailscale,
            "`tailscale status --json` printed something without a node state, so whether this machine is logged \
             in was not measured: run `tailscale status` by hand",
        ),
    }
}

/// The node state out of `tailscale status --json`, or `None` when the answer is not that shape.
///
/// JSON rather than the human text: the state is a field with a contract, and `tailscale status`'s prose is not
/// (ADR-0035's "a template asks for the field rather than parsing a sentence", applied to a status).
fn backend_state(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.get("BackendState")?.as_str().map(str::to_string)
}

/// The two listeners `serve` binds: free before a start, or held by this machine's own server after one.
///
/// `serving` is `None` for `--quick` and `Some(health)` for `--full`; see [`run`] for why the moment decides what
/// "in use" means.
fn ports(config: &Config, serving: Option<bool>) -> Vec<Finding> {
    vec![
        port(config.control_listen, "control", serving),
        port(config.preview_listen, "preview", serving),
    ]
}

/// One listener's answer.
fn port(listen: SocketAddr, label: &str, serving: Option<bool>) -> Finding {
    if listen.port() == 0 {
        return Finding::pass(
            Check::Ports,
            format!(
                "{label} {listen} asks the kernel for a port, so there is nothing here to hold"
            ),
        );
    }
    match TcpListener::bind(listen) {
        Ok(listener) => {
            drop(listener);
            Finding::pass(Check::Ports, format!("{label} {listen} is free"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => match serving {
            // Item 10 §7's moment: the deploy has just waited for `/api/health`, so an occupied port is the
            // machine serving rather than somebody else's process.
            Some(true) => Finding::pass(
                Check::Ports,
                format!(
                    "{label} {listen} is held by this machine's own `serve` (`GET /api/health` answers), which is \
                     what a serving machine looks like"
                ),
            ),
            _ => Finding::fail(
                Check::Ports,
                format!(
                    "{label} {listen} is in use ({error}): stop whatever holds it — `ss -ltnp` names the process — \
                     or move this listener to another port in the config file"
                ),
            ),
        },
        Err(error) => Finding::fail(
            Check::Ports,
            format!(
                "{label} {listen} cannot be bound ({error}): check that the address is this machine's and that \
                 the user may bind it, or change the listener in the config file"
            ),
        ),
    }
}

/// Whether `GET /api/health` answers 200 on `listen` — the one question that tells "this machine is serving"
/// from "something else holds the port".
///
/// Hand-written like [`crate::preflight`]'s `_ping`, and for the same reason: one request, four lines, no client
/// library. `/api/health` is guard-exempt, so no header but a `Host` is needed.
fn health_answers(listen: SocketAddr) -> bool {
    if listen.port() == 0 {
        return false;
    }
    let host = if listen.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        listen.ip()
    };
    let target = SocketAddr::new(host, listen.port());
    let Ok(mut stream) = TcpStream::connect_timeout(&target, PROBE_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    let request = b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    if stream.write_all(request).is_err() {
        return false;
    }
    let mut buffer = [0_u8; 1024];
    let Ok(read) = stream.read(&mut buffer) else {
        return false;
    };
    let head = String::from_utf8_lossy(&buffer[..read]);
    head.lines()
        .next()
        .is_some_and(|status| status.contains(" 200"))
}

/// The database's schema against this binary's, read **without** migrating it (ADR-0047).
///
/// `serve` needs no restore after a rollback, so it warns on a database a newer release wrote and carries on;
/// `doctor` is the guard that runs before a start, so it refuses. A machine that has never served has no file and
/// that is a pass — a first boot is not a problem to fix.
fn database(config: &Config) -> Finding {
    let path = config.data_root.join(db::DATABASE_FILE);
    match db::recorded_without_migrating(&path) {
        Ok(None) => Finding::pass(
            Check::Database,
            format!(
                "there is no database at {} yet, and `serve` creates and migrates one at startup",
                path.display()
            ),
        ),
        Ok(Some(recorded)) => match schema::verdict(recorded, schema::SCHEMA_VERSION) {
            // The one sentence, from the one comparison: `serve` warns with this string and `doctor` refuses with
            // it (HS-075, ADR-0047).
            Verdict::Newer => Finding::fail(
                Check::Database,
                Verdict::Newer.sentence(recorded, schema::SCHEMA_VERSION),
            ),
            verdict => Finding::pass(
                Check::Database,
                verdict.sentence(recorded, schema::SCHEMA_VERSION),
            ),
        },
        Err(error) => Finding::fail(
            Check::Database,
            format!(
                "the database at {} could not be read without migrating it ({error}): check that this user can \
                 read the data root, because `serve` cannot open it either",
                path.display()
            ),
        ),
    }
}

/// V2: `git merge-tree --write-tree` answers 0 for a clean merge and 1 with the path for a conflict.
///
/// The trial is a real repository — one commit, two branches that diverge on one file, and a third commit on the
/// other side that does not — because the only honest answer to "does this git do it" is to ask it. It lives in
/// `std::env::temp_dir()` and is removed whatever happened: `doctor` leaves no state behind (ADR-0054).
///
/// **The trial-merge fallback is not implemented**, and this is the decision ADR-0054 records rather than a gap:
/// the git check refuses anything older than [`preflight::MIN_GIT`], which is the release `--write-tree` arrived
/// in, so a git that lacks it never reaches this function at all — [`merge`]'s `git_ok` arm is `unknown`, and the
/// machine has already failed on `git`. The spec's "if the git version lacks it" therefore cannot happen on a
/// machine `doctor` would pass, which is the same shape as ADR-0035's unneeded V1 fallback.
fn merge(git: &Path, git_ok: bool) -> Finding {
    if !git_ok {
        return Finding::unknown(
            Check::Merge,
            format!(
                "not measured: the git check above failed, so this git's `merge-tree --write-tree` was not tried \
                 — it is the {}.{} release that arrived in, and the trial-merge fallback is not implemented \
                 because a git older than that cannot run item 12's mergeability check at all (ADR-0054)",
                preflight::MIN_GIT.0,
                preflight::MIN_GIT.1
            ),
        );
    }
    let scratch = std::env::temp_dir().join(format!("holosmith-doctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let trial = trial(git, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    match trial {
        Ok(None) => Finding::pass(
            Check::Merge,
            "`git merge-tree --write-tree` answered 0 for a clean merge and 1 with the path for a conflict",
        ),
        Ok(Some(said)) => Finding::fail(
            Check::Merge,
            format!(
                "`git merge-tree --write-tree` does not behave as item 12 needs: {said} — upgrade git, or install a \
                 build that has merge-tree"
            ),
        ),
        Err(said) => Finding::unknown(
            Check::Merge,
            format!(
                "the merge trial could not run in {} ({said}): this is not a verdict about mergeability, and \
                 `git` needs a writable temporary directory",
                scratch.display()
            ),
        ),
    }
}

/// The two measurements, as `Ok(None)` when they behave, `Ok(Some(sentence))` when they answered something else,
/// and `Err(sentence)` when the trial could not run at all.
fn trial(git: &Path, dir: &Path) -> Result<Option<String>, String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    // `init -b` needs 2.28, which the version check above has already established.
    git_ok(
        git,
        dir,
        &with_identity(&["init", "--quiet", "-b", "main", "."]),
    )?;
    write(dir, MERGE_FILE, "base\n")?;
    git_ok(git, dir, &with_identity(&["add", MERGE_FILE]))?;
    git_ok(
        git,
        dir,
        &with_identity(&["commit", "--quiet", "-m", "base"]),
    )?;
    // Two branches that change the same line, which is the conflict; and a commit on `main` that changes nothing
    // the other branch touched, which is the clean merge.
    git_ok(
        git,
        dir,
        &with_identity(&["checkout", "--quiet", "-b", "left"]),
    )?;
    write(dir, MERGE_FILE, "left\n")?;
    git_ok(
        git,
        dir,
        &with_identity(&["commit", "--quiet", "-a", "-m", "left"]),
    )?;
    git_ok(git, dir, &with_identity(&["checkout", "--quiet", "main"]))?;
    git_ok(
        git,
        dir,
        &with_identity(&["checkout", "--quiet", "-b", "right"]),
    )?;
    write(dir, MERGE_FILE, "right\n")?;
    git_ok(
        git,
        dir,
        &with_identity(&["commit", "--quiet", "-a", "-m", "right"]),
    )?;
    git_ok(git, dir, &with_identity(&["checkout", "--quiet", "main"]))?;
    write(dir, MERGE_ADDED, "added\n")?;
    git_ok(git, dir, &with_identity(&["add", MERGE_ADDED]))?;
    git_ok(
        git,
        dir,
        &with_identity(&["commit", "--quiet", "-m", "added"]),
    )?;

    let clean = git_output(
        git,
        dir,
        &with_identity(&["merge-tree", "--write-tree", "main", "left"]),
    )?;
    let conflict = git_output(
        git,
        dir,
        &with_identity(&["merge-tree", "--write-tree", "left", "right"]),
    )?;
    let clean_code = clean.status.code().unwrap_or(-1);
    let conflict_code = conflict.status.code().unwrap_or(-1);
    let clean_out = String::from_utf8_lossy(&clean.stdout);
    let conflict_out = String::from_utf8_lossy(&conflict.stdout);
    if clean_code != 0 {
        return Ok(Some(format!(
            "a merge that does not conflict answered {clean_code} ({})",
            tail(&clean_out)
        )));
    }
    if clean_out.trim().lines().count() != 1 {
        return Ok(Some(format!(
            "a clean merge printed {} lines where the tree's id is one: {}",
            clean_out.trim().lines().count(),
            tail(&clean_out)
        )));
    }
    if conflict_code != 1 || !conflict_out.contains(MERGE_FILE) {
        return Ok(Some(format!(
            "a conflict answered {conflict_code} with {:?} where 1 and the path {MERGE_FILE} were expected",
            tail(&conflict_out)
        )));
    }
    Ok(None)
}

/// The trial's git flags: an identity (a machine may have none configured), no hooks (a global `core.hooksPath` is
/// somebody else's code, and the trial commits) and no signing (a machine that signs every commit by default would
/// otherwise turn "this git can merge" into "this machine has a GPG agent running").
const IDENTITY: [&str; 10] = [
    "-c",
    "user.name=holosmith",
    "-c",
    "user.email=doctor@holosmith.invalid",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "tag.gpgsign=false",
];

/// [`IDENTITY`] followed by one git command.
fn with_identity<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    let mut all: Vec<&'a str> = IDENTITY.to_vec();
    all.extend_from_slice(rest);
    all
}

/// One `git` invocation inside the trial: `git -C <dir> <arguments>`.
fn git_output(git: &Path, dir: &Path, arguments: &[&str]) -> Result<std::process::Output, String> {
    Command::new(git)
        .arg("-C")
        .arg(dir)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| format!("`git {}` could not be run: {error}", arguments.join(" ")))
}

/// The same, insisting on a zero exit and naming git's own answer when it is not.
fn git_ok(git: &Path, dir: &Path, arguments: &[&str]) -> Result<(), String> {
    let output = git_output(git, dir, arguments)?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "`git {}` answered {} ({})",
        arguments.join(" "),
        output.status.code().unwrap_or(-1),
        tail(&String::from_utf8_lossy(&output.stderr))
    ))
}

/// A few words of a git answer, for a sentence: one line, bounded.
fn tail(text: &str) -> String {
    let said = text.trim();
    let first = said.lines().next().unwrap_or_default();
    if first.chars().count() > 200 {
        first.chars().take(200).collect()
    } else {
        first.to_string()
    }
}

/// A file the trial needs.
fn write(dir: &Path, name: &str, body: &str) -> Result<(), String> {
    std::fs::write(dir.join(name), body)
        .map_err(|error| format!("{name} could not be written: {error}"))
}

/// The machine's network isolation, measured by the self-test (ADR-0044) — whose **bare probe is the
/// network-off probe**: a container on a temporary internal network that reaches neither the internet, nor the
/// host, nor the egress proxy's alias. The proxy-joined pass is [`selftest::pair`], a `debug`/`warn` line rather
/// than a verdict, which is why one outcome speaks for both halves here.
fn network(machine: &dyn Machine, image: &str, docker_ok: bool) -> Finding {
    if !docker_ok {
        return Finding::unknown(
            Check::Network,
            not_measured("the isolation and the network-off probe need a container"),
        );
    }
    match machine.selftest(image) {
        selftest::Outcome::Passed => Finding::pass(
            Check::Network,
            "a probe container on a temporary internal network reached none of the ways out, and the network it \
             used was removed",
        ),
        selftest::Outcome::Failed(sentence) => Finding::fail(Check::Network, sentence),
        selftest::Outcome::Unknown(sentence) => Finding::unknown(Check::Network, sentence),
    }
}

/// Whether one of the machine's proxies is up. Read, never started: `serve`'s boot owns them (ADR-0045/0048).
fn proxy(machine: &dyn Machine, name: &str, alias: &str, docker_ok: bool) -> Finding {
    if !docker_ok {
        return Finding::unknown(
            Check::Proxy,
            not_measured(&format!("whether {name} is running needs the daemon")),
        );
    }
    if machine.running(name) {
        return Finding::pass(
            Check::Proxy,
            format!("{name} is running on the `egress` network, resolved by sessions as `{alias}`"),
        );
    }
    Finding::fail(
        Check::Proxy,
        format!(
            "{name} is not running: the machine's `serve` starts it on every Docker boot (spec item 10 §4), so \
             restart the server — or `docker rm -f {name}` and let the next boot create it"
        ),
    )
}

/// Whether the configured agent image runs the pi this binary is built for (spec's build-and-release note:
/// "`doctor` fails if the agent image's pi differs from it").
///
/// The same two functions HS-031 asserts with at a launch — [`agent::version_of`] and [`agent::version_check`] —
/// asked on demand, so the sentence an operator reads here is the one a session would fail with.
fn image_check(machine: &dyn Machine, image: &str, docker_ok: bool) -> Finding {
    if !docker_ok {
        return Finding::unknown(
            Check::Image,
            not_measured("asking the image for its pi version needs the daemon"),
        );
    }
    match machine.image_pi(image) {
        Ok(found) => match agent::version_check(image, &found) {
            Ok(found) => Finding::pass(
                Check::Image,
                format!("the agent image {image} runs pi {found}, which is the pinned one"),
            ),
            Err(sentence) => Finding::fail(Check::Image, sentence),
        },
        Err(said) => Finding::fail(
            Check::Image,
            format!(
                "the agent image {image} did not run ({said}): build it with this release's image step (HS-048), or \
                 point `agent_image` at an image that has pi"
            ),
        ),
    }
}

/// The one sentence a Docker-dependent check answers with when the daemon is the reason it was not asked.
fn not_measured(what: &str) -> String {
    format!(
        "{what}, and the docker check above failed: this is not a verdict about the machine, and a daemon that \
         answers `_ping` is the fix"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_depth_prints_the_specs_word() {
        assert_eq!(Depth::Quick.as_str(), "quick");
        assert_eq!(Depth::Full.to_string(), "full");
    }

    #[test]
    fn the_report_prints_one_line_per_finding_and_its_tally() {
        let report = Report {
            depth: Depth::Quick,
            findings: vec![
                Finding::pass(Check::Docker, "the daemon answers"),
                Finding::fail(Check::Git, "git is old: upgrade git"),
                Finding::unknown(Check::Tailscale, "no daemon"),
            ],
        };
        let text = report.render();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "doctor: quick");
        assert_eq!(lines[1], "docker: pass: the daemon answers");
        assert_eq!(lines[2], "git: fail: git is old: upgrade git");
        assert_eq!(lines[3], "tailscale: unknown: no daemon");
        assert_eq!(lines[4], "doctor: 3 findings: 1 pass, 1 fail, 1 unknown");
        assert_eq!(lines.len(), 5);
        assert!(report.failed());
        assert_eq!(report.failures().count(), 1);
    }

    #[test]
    fn a_report_with_no_failure_exits_zero() {
        // ADR-0044's rule, as the exit code: "could not measure" is not a failure, or a machine with no
        // Tailscale could not run `doctor` at all.
        let report = Report {
            depth: Depth::Quick,
            findings: vec![
                Finding::pass(Check::Docker, "the daemon answers"),
                Finding::unknown(Check::Tailscale, "no daemon"),
            ],
        };
        assert!(!report.failed());
        assert_eq!(report.failures().count(), 0);
    }

    #[test]
    fn a_systemd_that_is_not_there_is_unknown_and_one_that_is_is_a_pass() {
        let dir =
            std::env::temp_dir().join(format!("holosmith-doctor-systemd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let missing = systemd(&dir);
        assert_eq!(missing.outcome, Outcome::Unknown);
        assert!(
            missing.sentence.contains("server deploy"),
            "{}",
            missing.sentence
        );
        std::fs::create_dir_all(&dir).expect("the scratch directory is creatable");
        let present = systemd(&dir);
        assert_eq!(present.outcome, Outcome::Pass);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tailscale_json_answer_is_read_as_its_node_state() {
        let running = r#"{"BackendState":"Running","Self":{"HostName":"h"}}"#;
        assert_eq!(backend_state(running).as_deref(), Some("Running"));
        assert_eq!(
            backend_state(r#"{"BackendState": "NeedsLogin"}"#).as_deref(),
            Some("NeedsLogin")
        );
        assert_eq!(backend_state("{}"), None);
        assert_eq!(backend_state("not json"), None);
    }

    #[test]
    fn a_free_port_is_a_pass_and_a_held_one_names_the_fix() {
        // A port the kernel gave us, held for the length of the test: the fail arm is the one `ExecStartPre`
        // cares about, and 0 is the vacuous arm this asserts is not.
        let held = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
        let listen = held.local_addr().expect("the bound address is known");
        let config = Config {
            control_listen: listen,
            preview_listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            ..Config::default()
        };
        let findings = ports(&config, None);
        assert_eq!(findings[0].outcome, Outcome::Fail);
        assert!(
            findings[0].sentence.contains("in use"),
            "{}",
            findings[0].sentence
        );
        assert!(
            findings[0].sentence.contains("ss -ltnp"),
            "the failure names no way to find the holder: {}",
            findings[0].sentence
        );
        // Port 0 is "the kernel will give me one", which is not a port that can be held.
        assert_eq!(findings[1].outcome, Outcome::Pass);
        assert!(findings[1].sentence.contains("nothing here to hold"));

        // The item 10 §7 moment: with this machine's own health answering, the same held port is the pass.
        drop(held);
        assert_eq!(
            port(listen, "control", Some(true)).outcome,
            Outcome::Pass,
            "a serving machine's own port must not be a failure"
        );
        // …and a held port on a machine that is *not* serving is the failure, which is what makes the line above
        // meaningful.
        let held = TcpListener::bind(listen).expect("the port is bindable again");
        assert_eq!(port(listen, "control", Some(false)).outcome, Outcome::Fail);
        drop(held);
        assert_eq!(
            port(listen, "control", None).outcome,
            Outcome::Pass,
            "the port is free again, so the last check would be vacuous if it still failed"
        );
    }

    #[test]
    fn health_is_measured_and_not_assumed() {
        // A listener that answers `/api/health` with a 200 and one that answers nothing: the two answers the
        // `--full` moment turns on.
        use std::net::TcpListener as Listener;
        let good = Listener::bind("127.0.0.1:0").expect("a port is bindable");
        let good_address = good.local_addr().expect("the address is known");
        let thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = good.accept() {
                let mut buffer = [0_u8; 512];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
            }
        });
        assert!(health_answers(good_address));
        thread.join().expect("the answering thread finishes");

        let silent = Listener::bind("127.0.0.1:0").expect("a port is bindable");
        let silent_address = silent.local_addr().expect("the address is known");
        assert!(
            !health_answers(silent_address),
            "a listener that says nothing is not a serving machine"
        );
        drop(silent);
        assert!(
            !health_answers(silent_address),
            "a port nothing listens on is not a serving machine either"
        );
    }

    #[test]
    fn a_docker_dependent_check_without_a_daemon_is_not_a_verdict() {
        let unknown = not_measured("the isolation needs a container");
        assert!(unknown.contains("docker check"), "{unknown}");
        // The reason this matters: `serve` may run a `stub` backend on a machine with no daemon, and `doctor`
        // must still print its list rather than claiming a leak.
        let finding = network(&NoDaemon, "image", false);
        assert_eq!(finding.outcome, Outcome::Unknown);
    }

    #[test]
    fn a_missing_image_names_the_build_and_the_key() {
        let finding = image_check(&NoDaemon, "agent-image:test", true);
        assert_eq!(finding.outcome, Outcome::Fail);
        assert!(
            finding.sentence.contains("agent-image:test"),
            "{}",
            finding.sentence
        );
        assert!(
            finding.sentence.contains("agent_image"),
            "{}",
            finding.sentence
        );
    }

    /// A machine whose container questions all answer "nothing there" — the daemon-less half of a machine.
    struct NoDaemon;

    impl Machine for NoDaemon {
        fn running(&self, _name: &str) -> bool {
            false
        }

        fn image_pi(&self, _image: &str) -> Result<String, String> {
            Err("no such image".to_string())
        }

        fn selftest(&self, _image: &str) -> selftest::Outcome {
            selftest::Outcome::Unknown("no image".to_string())
        }
    }
}
