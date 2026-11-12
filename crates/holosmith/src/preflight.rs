//! The preflight: what this machine must have before it can run a session (HS-024, spec item 10 §1).
//!
//! Item 10 §1 puts the same checks in two places — `holosmith doctor --quick`, which the unit file runs
//! as `ExecStartPre` — and that is deliberate: a machine that fails one of them must fail *at startup*,
//! not halfway through a turn, and the sentence the operator reads must be the same one either way. This
//! module is the list and the sentences; HS-045's `doctor` is the command that prints them on demand, and
//! `serve` is the one that refuses.
//!
//! Three checks are answered here, because they are the three `serve` refuses on. Everything else HS-045's
//! `doctor` asks — systemd, the merge trial, Tailscale, the ports, the database's schema, the proxies and the
//! image's pi version — lives in [`crate::doctor`], which assembles those three and the rest into one report.
//! The network self-test needs a container and lives in [`crate::selftest`]; it reports through the same
//! [`Complaint`] and refuses the same way, so a reader of one has read both (ADR-0044).
//!
//! **One check, one function, two arms.** A check answers a [`Finding`] — the check's word, whether it passed,
//! failed or could not be measured, and the sentence the operator reads — and [`Complaint`] is that finding's
//! *failing projection*. That is how `serve`'s refusal and `doctor`'s line keep **the same sentence string**:
//! there is one place per check that writes it, and no second table of pass-words that could drift (ADR-0028,
//! amended by ADR-0054).
//!
//! - **Docker** — the daemon answers `GET /_ping` on the socket `docker_host` names. `unix://` (the
//!   default `/var/run/docker.sock`) and `tcp://host:port` are checked; `ssh://` is reported as *not
//!   checked*, because a check that cannot run must not report success. No story owns an ssh check yet:
//!   when one does, it replaces that complaint with a real one.
//! - **Git** — `git --version` is 2.38 or newer, which is the release `git merge-tree --write-tree`
//!   arrived in (V2's mergeability check runs it).
//! - **Disk** — the data root's filesystem has `min_free_gb` free. Measured with `df -Pk` rather than
//!   `statvfs`: the crate denies `unsafe`, so the syscall is not reachable without a `libc` dependency, and
//!   a POSIX `df` is on every machine item 10 §1 describes (Linux with coreutils). A `df` that cannot be
//!   run is a complaint, not a pass — the same rule as `ssh://`.
//!
//! **Every failure is one sentence that names the fix**, and all of them are reported at once: a machine
//! missing two things should be fixable with two edits, not with two restarts (ADR-0006's rule for config,
//! applied to a machine).
//!
//! The whole thing is synchronous and dependency-free on purpose. It runs once, before the listener binds,
//! so an `async` task would buy nothing; `std`'s sockets and `Command` are what a startup question needs.

use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::config::Config;

/// How long the daemon has to answer `/_ping` before it is "not reachable".
///
/// A daemon that is starting up answers in milliseconds; one that is wedged answers never, and a startup
/// that waits for it is the failure this preflight exists to prevent.
const PING_TIMEOUT: Duration = Duration::from_secs(3);

/// The default socket a rootful daemon listens on, used when `docker_host` is unset.
pub const DEFAULT_DOCKER_HOST: &str = "unix:///var/run/docker.sock";

/// The oldest git the merge check works with: `merge-tree --write-tree` first shipped in 2.38.
pub const MIN_GIT: (u64, u64) = (2, 38);

/// Which check spoke. It is the word before the sentence in every report, so an operator with two
/// complaints can tell which machine part each one is about.
///
/// **One table of words, for every check the build has** — not one here and another in `doctor`. A word is a
/// fact an operator greps (`docker`, `git`, …), so a second spelling of one would be a second way to be wrong;
/// the same reasoning `Check::Network` already carried. [`check`] answers [`Docker`](Self::Docker),
/// [`Git`](Self::Git) and [`Disk`](Self::Disk); [`crate::selftest`] answers [`Network`](Self::Network);
/// [`crate::doctor`] answers the rest and prints all of them beside these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Docker,
    Git,
    Disk,
    /// The machine's network isolation, measured by [`crate::selftest`] rather than by a command here.
    ///
    /// It is in this enum because it is reported the same way and refuses the same way, and because HS-045's
    /// `doctor` has to print it beside the other three. [`check`] never answers it: it needs a container, so it
    /// lives in its own module and `serve` adds its complaint to this list.
    Network,
    /// Whether this machine has systemd (`/run/systemd/system`), which `server deploy` needs and `serve` by
    /// hand does not.
    Systemd,
    /// Whether git's `merge-tree --write-tree` behaves — V2's measurement, run by [`crate::doctor`].
    Merge,
    /// Whether Tailscale is installed and logged in, which a hub needs and a LAN-only machine does not.
    Tailscale,
    /// Whether the control and preview ports are free (before a start) or held by this machine's own `serve`
    /// (after one).
    Ports,
    /// The database's schema against this binary's, read without migrating it (ADR-0047).
    Database,
    /// The installed paths' owners and modes — the one check the deploy's own output must satisfy (HS-049).
    Perms,
    /// Whether the machine's `hs_egress`/`hs_key` proxies are up.
    Proxy,
    /// Whether the configured agent image runs the pinned pi.
    Image,
    /// Whether `openssl` runs, which the Web Push sender needs (HS-066) and nothing on the request path does.
    ///
    /// It is in this enum because it is reported like every other machine question and only `doctor` asks it: no
    /// startup check runs it, because a missing notification channel must not refuse a boot (ADR-0071's D9).
    Openssl,
    /// The release tree: what `current` resolves to, and whether the unit can execute through it (HS-080).
    ///
    /// One word for one question, so an operator greps `release` and finds the tree rather than a `version` line. It is
    /// asked in **both** depths — `--quick` is the unit's `ExecStartPre`, and a `current` that resolves to nothing is
    /// a machine systemd cannot start (see [`crate::doctor`]).
    Release,
}

impl Check {
    /// The word the report prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Git => "git",
            Self::Disk => "disk",
            Self::Network => "network",
            Self::Systemd => "systemd",
            Self::Merge => "merge",
            Self::Tailscale => "tailscale",
            Self::Ports => "ports",
            Self::Database => "database",
            Self::Perms => "perms",
            Self::Proxy => "proxy",
            Self::Image => "image",
            Self::Openssl => "openssl",
            Self::Release => "release",
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing this machine cannot do, as the operator has to read it: which check, and one sentence naming
/// the fix.
///
/// The sentence is *data* as well as text, because two readers print it: `serve`, which then refuses, and
/// HS-045's `doctor`, which prints the same words on demand. A `DockerBackend` carrying it would be a third
/// opinion about a machine the startup already refused, which is why it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Complaint {
    pub check: Check,
    pub sentence: String,
}

impl Complaint {
    /// The failing projection of a finding, which is the only outcome `serve` refuses on.
    ///
    /// It is a projection rather than a second constructor table: a check writes its sentence **once**, and
    /// `doctor`'s line and this complaint carry the same string (ADR-0028, ADR-0054).
    pub fn of(finding: &Finding) -> Option<Self> {
        finding.is_failure().then(|| Self {
            check: finding.check,
            sentence: finding.sentence.clone(),
        })
    }
}

impl fmt::Display for Complaint {
    /// `docker: start it with …` — the check, then the sentence.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.check, self.sentence)
    }
}

/// What a check found, from the operator's side: the two verdicts and the one honest third.
///
/// Only [`Outcome::Fail`] is a reason not to run: an `unknown` is *"this check did not produce a verdict about
/// whether `serve` may run here"*, and its sentence says what was missing and what does need it. That is
/// ADR-0044's rule one level up — "we could not measure" is not "we measured a leak" — and it is what keeps
/// `doctor --quick` runnable on a machine that has no systemd and no Tailscale daemon, where `serve` by hand is
/// still supported (ADR-0054).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    Unknown,
}

impl Outcome {
    /// The word the report prints after the check.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One check's answer: the check, its outcome, and the sentence the operator reads.
///
/// `serve` prints only the failing ones (as a [`Complaint`]); `doctor` prints all of them, which is what the
/// acceptance's "lists each check with pass/fail" is. The sentence is the same value in both, so the two
/// commands cannot disagree about a machine (ADR-0028).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub check: Check,
    pub outcome: Outcome,
    pub sentence: String,
}

impl Finding {
    pub fn pass(check: Check, sentence: impl Into<String>) -> Self {
        Self {
            check,
            outcome: Outcome::Pass,
            sentence: sentence.into(),
        }
    }

    pub fn fail(check: Check, sentence: impl Into<String>) -> Self {
        Self {
            check,
            outcome: Outcome::Fail,
            sentence: sentence.into(),
        }
    }

    pub fn unknown(check: Check, sentence: impl Into<String>) -> Self {
        Self {
            check,
            outcome: Outcome::Unknown,
            sentence: sentence.into(),
        }
    }

    pub fn is_failure(&self) -> bool {
        self.outcome == Outcome::Fail
    }
}

impl fmt::Display for Finding {
    /// `docker: pass: the daemon at … answers /_ping` — the check, the outcome, the sentence.
    ///
    /// Three colon-separated fields rather than padding, because a gate greps `^git: pass: ` and an operator
    /// reads the check first.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.check, self.outcome, self.sentence)
    }
}

/// The external programs this build runs under a test's control, as paths.
///
/// Three checks are a program's answer, and this is the seam that lets a test point them at a shim: the
/// production value is the name, which the machine's `PATH` resolves, and a test's is the shim it wrote. It is
/// a parameter rather than an environment variable because this crate denies `unsafe`, so `set_var` is not
/// reachable — and because an environment a second test could see is a flake waiting to happen.
///
/// `git` and `df` are the startup check's (HS-024) and `df` is the disk floor's, on both request paths
/// (HS-062); `du` is the per-session soft cap's watchdog (HS-063), which is neither a request path nor a
/// startup check — so the value rides here because this is the one place a test can own a program. `openssl` is
/// the Web Push sender's (HS-066), a background task's program like `du`, and it is here for the same reason.
/// `tailscale` is machine-to-machine trust's (HS-070): the whois both listeners run on a request path, which a
/// test can only stand in for by naming a shim.
#[derive(Debug, Clone)]
pub struct Tools {
    pub git: PathBuf,
    pub df: PathBuf,
    pub du: PathBuf,
    pub openssl: PathBuf,
    pub tailscale: PathBuf,
}

impl Default for Tools {
    /// `git`, `df`, `du`, `openssl` and `tailscale` from `PATH`, which is what a machine has.
    fn default() -> Self {
        Self {
            git: PathBuf::from("git"),
            df: PathBuf::from(crate::disk::DF),
            du: PathBuf::from(crate::quota::DU),
            openssl: PathBuf::from(crate::push::OPENSSL),
            tailscale: PathBuf::from(crate::trust::PROGRAM),
        }
    }
}

/// Every check this machine fails, in one list. Empty means the machine can run a session.
///
/// The caller decides *whether* to ask, and nothing here asks: a `stub` run needs no daemon, no container
/// and no free space beyond its own database, so `serve` calls this only when the run's backend is Docker and
/// its role is not `hub` — a machine that runs no sessions is not checked at all. Every check here is about a
/// machine that will actually run one.
pub fn check(config: &Config) -> Vec<Complaint> {
    check_with(config, &Tools::default())
}

/// [`check`], with the two external programs named.
///
/// The tests point these at scripts they wrote; nothing else does. It is public because the crate's tests
/// live outside it, the same reason `Deleter::remove` and `Turn::run` are.
pub fn check_with(config: &Config, tools: &Tools) -> Vec<Complaint> {
    report_with(config, tools)
        .iter()
        .filter_map(Complaint::of)
        .collect()
}

/// Every check this module answers, passed or failed, in the order they are printed.
///
/// [`check`] is its failures: one list, one spelling of each sentence, so `serve`'s refusal and `doctor`'s report
/// cannot say different things about one machine (ADR-0028, ADR-0054).
pub fn report_with(config: &Config, tools: &Tools) -> Vec<Finding> {
    vec![daemon(config), git(&tools.git), disk(config, &tools.df)]
}

/// Whether the daemon answers, asked on its own — the check `holosmith status`'s two proxy lines rest on (HS-050).
///
/// One function, two callers: [`report_with`]'s list and `status`, which asks it **first** so that a daemon nobody
/// could reach makes both proxy lines `unknown:` instead of `not running`. That distinction is the whole of ADR-0054
/// applied to this read — `Docker::running` folds every daemon error into `false`, so asking it alone would turn "I
/// could not look" into a measurement, and `doctor` has made the same call for its own proxy findings.
pub fn daemon(config: &Config) -> Finding {
    docker(config.docker_host.as_deref())
}

/// `GET /_ping` against the daemon, over the socket `docker_host` names.
fn docker(docker_host: Option<&str>) -> Finding {
    let host = docker_host.unwrap_or(DEFAULT_DOCKER_HOST);
    match host.split_once("://") {
        Some(("unix", path)) => match ping_unix(path) {
            Ok(()) => Finding::pass(
                Check::Docker,
                format!("the daemon at {host} answers `GET /_ping`"),
            ),
            Err(reason) => Finding::fail(Check::Docker, socket_sentence(host, path, &reason)),
        },
        Some(("tcp", authority)) => match ping_tcp(authority) {
            Ok(()) => Finding::pass(
                Check::Docker,
                format!("the daemon at {host} answers `GET /_ping`"),
            ),
            Err(reason) => Finding::fail(
                Check::Docker,
                format!(
                    "the docker daemon at {host} did not answer ({}): start it on that host, or set \
                     docker_host to a socket this machine can reach",
                    reason.message
                ),
            ),
        },
        Some(("ssh", _)) => Finding::fail(
            Check::Docker,
            format!(
                "{host} is an ssh host, which this build cannot check: run the daemon on this machine and set \
                 docker_host to a socket it can reach"
            ),
        ),
        _ => Finding::fail(
            Check::Docker,
            format!(
                "docker_host {host:?} is not a socket this build can use: write it as \
                 `unix:///var/run/docker.sock` or `tcp://host:port`"
            ),
        ),
    }
}

/// What a unix socket's refusal means, as the fix.
///
/// **`permission denied` does not have the stopped daemon's fix.** A socket this user may not open is a
/// group the operator is not in — the socket is answering, it just will not talk — so that sentence names
/// `usermod`, not `systemctl`. Every other reason is the daemon not being there or not being healthy, and
/// `systemctl start` is the fix for those.
fn socket_sentence(host: &str, path: &str, reason: &Unreachable) -> String {
    if reason.kind == ErrorKind::PermissionDenied {
        return format!(
            "the docker socket {path} refused this user ({}): add yourself to the docker group with \
             `sudo usermod -aG docker $USER` and log back in, or set docker_host to a socket you can open",
            reason.message
        );
    }
    format!(
        "the docker daemon at {host} did not answer ({}): start it with `sudo systemctl start docker`, or \
         set docker_host to the daemon you run",
        reason.message
    )
}

/// Why a socket did not answer, as the sentence needs it: the message, and the kind when the fix depends
/// on it (`permission denied` is a group, not a stopped daemon).
struct Unreachable {
    message: String,
    kind: ErrorKind,
}

impl Unreachable {
    fn of(error: std::io::Error) -> Self {
        Self {
            message: error.to_string(),
            kind: error.kind(),
        }
    }

    /// A reason no `io::Error` produced — a socket that answered the wrong thing, or nothing.
    fn said(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ErrorKind::Other,
        }
    }
}

/// The daemon's answer to `/_ping`, over a unix socket.
fn ping_unix(path: &str) -> Result<(), Unreachable> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path).map_err(Unreachable::of)?;
    stream
        .set_read_timeout(Some(PING_TIMEOUT))
        .map_err(Unreachable::of)?;
    stream
        .set_write_timeout(Some(PING_TIMEOUT))
        .map_err(Unreachable::of)?;
    ping(&mut stream)
}

/// The daemon's answer to `/_ping`, over TCP (`docker -H tcp://…`, or a host with `dockerd -H`).
fn ping_tcp(authority: &str) -> Result<(), Unreachable> {
    use std::net::{TcpStream, ToSocketAddrs};
    let address = authority
        .to_socket_addrs()
        .map_err(|error| Unreachable::said(format!("{authority} is not an address: {error}")))?
        .next()
        .ok_or_else(|| Unreachable::said(format!("{authority} is not an address")))?;
    let mut stream = TcpStream::connect_timeout(&address, PING_TIMEOUT)
        .map_err(|error| Unreachable::said(format!("{address}: {error}")))?;
    stream
        .set_read_timeout(Some(PING_TIMEOUT))
        .map_err(Unreachable::of)?;
    ping(&mut stream)
}

/// One HTTP/1.1 `GET /_ping` and the answer's status line.
///
/// Hand-written rather than an HTTP client: the request is four lines, the daemon's answer to it is fixed
/// (Docker's own API documents `200 OK` with the body `OK`), and adding a client crate for one request
/// would be a dependency this binary carries for ever.
///
/// **One read, not "read until EOF".** Docker writes `_ping`'s whole answer in one go, `Connection: close`
/// included, so the first read holds it; a reader that waited for EOF would sit on its timeout against a
/// daemon that keeps the connection alive and report a healthy daemon as a wedged one. A socket that
/// accepts and answers nothing *is* the wedged daemon this check exists for, and the read timeout (not
/// `read_to_string`) is what turns it into a complaint — but note that it takes the timeout to do it, so a
/// listening socket that says nothing costs `PING_TIMEOUT` of startup.
fn ping(stream: &mut (impl std::io::Read + std::io::Write)) -> Result<(), Unreachable> {
    stream
        .write_all(b"GET /_ping HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n")
        .map_err(Unreachable::of)?;
    let mut buffer = [0_u8; 1024];
    let read = stream.read(&mut buffer).map_err(Unreachable::of)?;
    let answer = String::from_utf8_lossy(&buffer[..read]);
    let mut parts = answer.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let body = parts.next().unwrap_or_default().trim();
    let status = head
        .lines()
        .next()
        .ok_or_else(|| Unreachable::said("the daemon closed the connection without answering"))?;
    if !status.contains(" 200") {
        return Err(Unreachable::said(format!("the daemon answered {status:?}")));
    }
    // Docker's own contract is the body `OK`. A body that arrived in the same write and says something
    // else is a proxy answering for the daemon; a body that has not arrived yet is accepted, because the
    // status line is the daemon's.
    if !body.is_empty() && !body.eq_ignore_ascii_case("OK") {
        return Err(Unreachable::said(format!(
            "the daemon answered 200 with {body:?} instead of OK"
        )));
    }
    Ok(())
}

/// `git --version` at 2.38 or newer.
fn git(git: &Path) -> Finding {
    let output = match Command::new(git).arg("--version").output() {
        Ok(output) if output.status.success() => output,
        Ok(_) => {
            return Finding::fail(
                Check::Git,
                "git answered `git --version` with a failure: reinstall git 2.38 or newer",
            );
        }
        Err(error) => {
            return Finding::fail(
                Check::Git,
                format!(
                    "git is not runnable ({error}): install git 2.38 or newer, which is what \
                     `merge-tree --write-tree` needs"
                ),
            );
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    match parse_git_version(&text) {
        Some(version) if version >= MIN_GIT => Finding::pass(
            Check::Git,
            format!(
                "git {}.{} is {}.{} or newer, which is where `merge-tree --write-tree` arrived",
                version.0, version.1, MIN_GIT.0, MIN_GIT.1
            ),
        ),
        Some((major, minor)) => Finding::fail(
            Check::Git,
            format!(
                "git is {major}.{minor} and holosmith needs {}.{} or newer for `merge-tree --write-tree`: \
                 upgrade git",
                MIN_GIT.0, MIN_GIT.1
            ),
        ),
        None => Finding::fail(
            Check::Git,
            format!(
                "`git --version` printed {text:?}, which is not a git version: reinstall git {}.{} or newer",
                MIN_GIT.0, MIN_GIT.1
            ),
        ),
    }
}

/// `(major, minor)` out of `git version 2.43.0`, or `None` for anything else.
fn parse_git_version(text: &str) -> Option<(u64, u64)> {
    let version = text.trim().strip_prefix("git version ")?;
    let mut parts = version.split(['.', '-', ' ']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// `min_free_gb` free on the data root's filesystem.
///
/// The measurement itself lives in [`crate::disk`] since HS-062, because two more readers want the same
/// number: the two request paths that start work. What stays here is this check's **sentences** — four for the
/// four ways the measurement can fail to happen, and the two that name the numbers — which are the boot's and
/// `doctor`'s, not a create's (ADR-0068).
///
/// **A data root that is not there yet is not a disk problem**, and that arm is why `doctor` and `serve` can agree
/// about a fresh machine: `serve` creates the directory before it asks (`commands/serve.rs`), so on `serve`'s path
/// there is always a filesystem to measure — while `doctor --quick` is item 10 §1, which `server deploy` runs before
/// step 2 has created anything. `df` on a missing path is an error, so answering that error as *the disk is full*
/// would make a fresh host fail the check `serve` passes, and the sentence would name a fix that does not exist.
/// There is no filesystem to measure, so the outcome is `unknown` and the sentence says so (ADR-0054).
fn disk(config: &Config, df: &Path) -> Finding {
    let root = config.data_root.display();
    let available_gb = match crate::disk::measure(df, &config.data_root) {
        Ok(available_gb) => available_gb,
        Err(crate::disk::DiskError::Missing) => {
            return Finding::unknown(
                Check::Disk,
                format!(
                    "there is no data root at {root} yet, so there is no filesystem to measure and `serve` creates \
                     one at startup: this is not a disk problem, and `min_free_gb` is checked from the first boot \
                     on"
                ),
            );
        }
        // `df` ran and refused: a data root that does not exist, or one under a directory this user cannot
        // traverse. Its own words are the fix's context, so they are in the sentence — "install coreutils"
        // would be advice about a program that just answered.
        Err(crate::disk::DiskError::Refused(said)) => {
            return Finding::fail(
                Check::Disk,
                format!(
                    "`df -Pk {root}` failed ({said}), so the free space `min_free_gb` guards could not be \
                     measured: check that the data root exists and that this user can reach it"
                ),
            );
        }
        Err(crate::disk::DiskError::Unrunnable(error)) => {
            return Finding::fail(
                Check::Disk,
                format!(
                    "df is not runnable ({error}), so the free space `min_free_gb` guards could not be \
                     measured: install coreutils"
                ),
            );
        }
        Err(crate::disk::DiskError::Unprintable(text)) => {
            return Finding::fail(
                Check::Disk,
                format!(
                    "`df -Pk {root}` printed {text:?}, which is not a POSIX df answer: install coreutils"
                ),
            );
        }
    };
    match crate::disk::verdict(available_gb, config.min_free_gb) {
        crate::disk::Verdict::Enough => Finding::pass(
            Check::Disk,
            format!(
                "{root} has {available_gb} GB free and min_free_gb is {}",
                config.min_free_gb
            ),
        ),
        crate::disk::Verdict::Short => Finding::fail(
            Check::Disk,
            format!(
                "{root} has {available_gb} GB free and min_free_gb is {}: free some space, or lower min_free_gb \
                 if this machine really has room for fewer sessions",
                config.min_free_gb
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finding_prints_its_check_its_outcome_and_its_sentence() {
        let finding = Finding::fail(Check::Git, "git is old: upgrade git");
        assert_eq!(finding.to_string(), "git: fail: git is old: upgrade git");
        let passed = Finding::pass(Check::Docker, "the daemon answers");
        assert_eq!(passed.to_string(), "docker: pass: the daemon answers");
        assert_eq!(Check::Docker.as_str(), "docker");
        assert_eq!(Check::Disk.to_string(), "disk");
    }

    #[test]
    fn only_a_failure_becomes_the_complaint_serve_refuses_with() {
        // The rule ADR-0028 and ADR-0054 share: `serve` prints the same sentence `doctor` does, and an
        // `unknown` is not a reason to refuse a boot (ADR-0044).
        let failed = Finding::fail(Check::Git, "git is old: upgrade git");
        assert_eq!(
            Complaint::of(&failed),
            Some(Complaint {
                check: Check::Git,
                sentence: "git is old: upgrade git".to_string()
            })
        );
        assert_eq!(
            Complaint::of(&Finding::pass(Check::Git, "git is fine")),
            None
        );
        assert_eq!(
            Complaint::of(&Finding::unknown(Check::Tailscale, "no daemon")),
            None
        );
    }

    #[test]
    fn a_data_root_that_is_not_there_yet_is_not_a_disk_problem() {
        // `doctor --quick` is item 10 §1, which runs before the deploy creates anything, while `serve` creates the
        // directory before it asks. A `df` on the missing path is an error, and answering that as "the disk is
        // full" would make a fresh host fail the check `serve` passes (ADR-0054).
        let config = Config {
            data_root: std::env::temp_dir().join(format!(
                "holosmith-no-such-data-root-{}",
                std::process::id()
            )),
            ..Config::default()
        };
        let finding = disk(&config, Path::new("df-that-is-never-run"));
        assert_eq!(finding.outcome, Outcome::Unknown);
        assert!(finding.sentence.contains("serve"), "{}", finding.sentence);
        assert!(
            finding.sentence.contains("min_free_gb"),
            "{}",
            finding.sentence
        );
        assert!(!finding.sentence.contains("free some space"));
    }

    #[test]
    fn a_git_version_is_two_numbers_and_the_rest_is_ignored() {
        assert_eq!(parse_git_version("git version 2.43.0\n"), Some((2, 43)));
        assert_eq!(parse_git_version("git version 2.38.0.rc1"), Some((2, 38)));
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39))
        );
        // The version macOS' `/usr/bin/git` prints before the toolchain is installed.
        assert_eq!(
            parse_git_version("git version 2.20.1 (Apple Git-117)"),
            Some((2, 20))
        );
        assert_eq!(parse_git_version("git version 3"), None);
        assert_eq!(parse_git_version("svn, version 1.14"), None);
        assert_eq!(parse_git_version(""), None);
    }

    #[test]
    fn an_ssh_host_is_reported_as_unchecked_rather_than_passed() {
        // The rule this test exists for: a check that cannot run must not report success.
        let complaint = docker(Some("ssh://adam@host"));
        assert_eq!(complaint.check, Check::Docker);
        assert_eq!(complaint.outcome, Outcome::Fail);
        assert!(complaint.sentence.contains("ssh"), "{}", complaint.sentence);
        assert!(
            complaint.sentence.contains("cannot check"),
            "the sentence does not say it was not checked: {}",
            complaint.sentence
        );
    }

    #[test]
    fn a_socket_this_user_may_not_open_names_the_group_and_not_the_daemon() {
        // The commonest real failure on a working daemon: the socket is there and will not talk to this
        // user. `systemctl start` does not fix it, so the sentence must not say it.
        let reason = Unreachable {
            message: "Permission denied (os error 13)".to_string(),
            kind: ErrorKind::PermissionDenied,
        };
        let sentence = socket_sentence(
            "unix:///var/run/docker.sock",
            "/var/run/docker.sock",
            &reason,
        );
        assert!(
            sentence.contains("usermod -aG docker"),
            "the sentence names no group fix: {sentence}"
        );
        assert!(
            !sentence.contains("systemctl"),
            "a permission problem was answered with the stopped-daemon fix: {sentence}"
        );
    }

    #[test]
    fn a_socket_that_is_not_there_is_one_sentence_naming_the_fix() {
        let complaint = docker(Some("unix:///nonexistent-holosmith-test.sock"));
        assert_eq!(complaint.check, Check::Docker);
        assert_eq!(complaint.outcome, Outcome::Fail);
        assert!(
            complaint.sentence.contains("start it with"),
            "the sentence names no fix: {}",
            complaint.sentence
        );
        assert!(
            complaint.sentence.contains("docker_host"),
            "the sentence names no alternative: {}",
            complaint.sentence
        );
    }

    #[test]
    fn a_docker_host_that_is_not_a_scheme_is_refused_with_the_two_spellings() {
        let complaint = docker(Some("/var/run/docker.sock"));
        assert_eq!(complaint.outcome, Outcome::Fail);
        assert!(
            complaint.sentence.contains("unix://"),
            "{}",
            complaint.sentence
        );
        assert!(
            complaint.sentence.contains("tcp://"),
            "{}",
            complaint.sentence
        );
    }
}
