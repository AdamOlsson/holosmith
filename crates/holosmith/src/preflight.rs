//! The preflight: what this machine must have before it can run a session (HS-024, spec item 10 §1).
//!
//! Item 10 §1 puts the same checks in two places — `holosmith doctor --quick`, which the unit file runs
//! as `ExecStartPre` — and that is deliberate: a machine that fails one of them must fail *at startup*,
//! not halfway through a turn, and the sentence the operator reads must be the same one either way. This
//! module is the list and the sentences; HS-045's `doctor` is the command that prints them on demand, and
//! `serve` is the one that refuses.
//!
//! Three checks are answered here, and only these three, because they are what this build can answer without
//! Docker and without the systemd, Tailscale and port questions HS-045 owns. A fourth, the network self-test,
//! needs a container and lives in [`crate::selftest`]; it reports through the same [`Complaint`] and refuses
//! the same way, so a reader of one has read both (ADR-0044).
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
}

impl Check {
    /// The word the report prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Git => "git",
            Self::Disk => "disk",
            Self::Network => "network",
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
    fn docker(sentence: impl Into<String>) -> Self {
        Self {
            check: Check::Docker,
            sentence: sentence.into(),
        }
    }

    fn git(sentence: impl Into<String>) -> Self {
        Self {
            check: Check::Git,
            sentence: sentence.into(),
        }
    }

    fn disk(sentence: impl Into<String>) -> Self {
        Self {
            check: Check::Disk,
            sentence: sentence.into(),
        }
    }
}

impl fmt::Display for Complaint {
    /// `docker: start it with …` — the check, then the sentence.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.check, self.sentence)
    }
}

/// The external programs the preflight runs, as paths.
///
/// Two of the three checks are a program's answer, and this is the seam that lets a test point them at a
/// shim: the production value is the name, which the machine's `PATH` resolves, and a test's is the shim
/// it wrote. It is a parameter rather than an environment variable because this crate denies `unsafe`,
/// so `set_var` is not reachable — and because an environment a second test could see is a flake waiting
/// to happen.
#[derive(Debug, Clone)]
pub struct Tools {
    pub git: PathBuf,
    pub df: PathBuf,
}

impl Default for Tools {
    /// `git` and `df` from `PATH`, which is what a machine has.
    fn default() -> Self {
        Self {
            git: PathBuf::from("git"),
            df: PathBuf::from("df"),
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
    let mut complaints = Vec::new();
    if let Some(complaint) = docker(config.docker_host.as_deref()) {
        complaints.push(complaint);
    }
    if let Some(complaint) = git(&tools.git) {
        complaints.push(complaint);
    }
    if let Some(complaint) = disk(config, &tools.df) {
        complaints.push(complaint);
    }
    complaints
}

/// `GET /_ping` against the daemon, over the socket `docker_host` names.
fn docker(docker_host: Option<&str>) -> Option<Complaint> {
    let host = docker_host.unwrap_or(DEFAULT_DOCKER_HOST);
    match host.split_once("://") {
        Some(("unix", path)) => match ping_unix(path) {
            Ok(()) => None,
            Err(reason) => Some(Complaint::docker(socket_sentence(host, path, &reason))),
        },
        Some(("tcp", authority)) => match ping_tcp(authority) {
            Ok(()) => None,
            Err(reason) => Some(Complaint::docker(format!(
                "the docker daemon at {host} did not answer ({}): start it on that host, or set \
                 docker_host to a socket this machine can reach",
                reason.message
            ))),
        },
        Some(("ssh", _)) => Some(Complaint::docker(format!(
            "{host} is an ssh host, which this build cannot check: run the daemon on this machine and set \
             docker_host to a socket it can reach"
        ))),
        _ => Some(Complaint::docker(format!(
            "docker_host {host:?} is not a socket this build can use: write it as \
             `unix:///var/run/docker.sock` or `tcp://host:port`"
        ))),
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
fn git(git: &Path) -> Option<Complaint> {
    let output = match Command::new(git).arg("--version").output() {
        Ok(output) if output.status.success() => output,
        Ok(_) => {
            return Some(Complaint::git(
                "git answered `git --version` with a failure: reinstall git 2.38 or newer",
            ));
        }
        Err(error) => {
            return Some(Complaint::git(format!(
                "git is not runnable ({error}): install git 2.38 or newer, which is what \
                 `merge-tree --write-tree` needs"
            )));
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    match parse_git_version(&text) {
        Some(version) if version >= MIN_GIT => None,
        Some((major, minor)) => Some(Complaint::git(format!(
            "git is {major}.{minor} and holosmith needs {}.{} or newer for `merge-tree --write-tree`: \
             upgrade git",
            MIN_GIT.0, MIN_GIT.1
        ))),
        None => Some(Complaint::git(format!(
            "`git --version` printed {text:?}, which is not a git version: reinstall git {}.{} or newer",
            MIN_GIT.0, MIN_GIT.1
        ))),
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
fn disk(config: &Config, df: &Path) -> Option<Complaint> {
    let output = match Command::new(df).arg("-Pk").arg(&config.data_root).output() {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            // `df` ran and refused: a data root that does not exist, or one under a directory this user
            // cannot traverse. Its own words are the fix's context, so they are in the sentence — "install
            // coreutils" would be advice about a program that just answered.
            let said = String::from_utf8_lossy(&output.stderr);
            let said = said.trim();
            return Some(Complaint::disk(format!(
                "`df -Pk {}` failed ({said}), so the free space `min_free_gb` guards could not be \
                 measured: check that the data root exists and that this user can reach it",
                config.data_root.display()
            )));
        }
        Err(error) => {
            return Some(Complaint::disk(format!(
                "df is not runnable ({error}), so the free space `min_free_gb` guards could not be \
                 measured: install coreutils"
            )));
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(available_kb) = parse_df_available(&text) else {
        return Some(Complaint::disk(format!(
            "`df -Pk {}` printed {text:?}, which is not a POSIX df answer: install coreutils",
            config.data_root.display()
        )));
    };
    let available_gb = available_kb / (1024 * 1024);
    if available_gb >= config.min_free_gb {
        return None;
    }
    Some(Complaint::disk(format!(
        "{} has {available_gb} GB free and min_free_gb is {}: free some space, or lower min_free_gb \
         if this machine really has room for fewer sessions",
        config.data_root.display(),
        config.min_free_gb
    )))
}

/// The available-kilobytes column of a POSIX `df -Pk` answer: the fourth field of the second line.
///
/// `-P` is what makes it one line per filesystem, so the columns are fixed and no header text is read; the
/// filesystem name is a path and cannot contain a space in the answer `df -P` prints for a plain path.
fn parse_df_available(text: &str) -> Option<u64> {
    let line = text.lines().nth(1)?;
    line.split_whitespace().nth(3)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complaint_prints_its_check_and_its_sentence() {
        let complaint = Complaint::git("git is old: upgrade git");
        assert_eq!(complaint.to_string(), "git: git is old: upgrade git");
        assert_eq!(Check::Docker.as_str(), "docker");
        assert_eq!(Check::Disk.to_string(), "disk");
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
    fn a_posix_df_answer_is_the_fourth_field_of_its_second_line() {
        // `df -Pk` on Linux, with a long device name and a long path, which is what pushes the columns.
        let answer = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                      /dev/mapper/vg-root 1000000000 500000000 480000000 52% /\n";
        assert_eq!(parse_df_available(answer), Some(480_000_000));
        assert_eq!(
            parse_df_available("Filesystem 1024-blocks Used Available Capacity Mounted on\n"),
            None
        );
        assert_eq!(parse_df_available(""), None);
        // A wrapped device name is not a POSIX answer, and reading it as one would report a filesystem that
        // does not exist: `-P` is what makes the second line the answer's only one.
        assert_eq!(
            parse_df_available(
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/mapper/a\n"
            ),
            None
        );
    }

    #[test]
    fn an_ssh_host_is_reported_as_unchecked_rather_than_passed() {
        // The rule this test exists for: a check that cannot run must not report success.
        let complaint =
            docker(Some("ssh://adam@host")).expect("ssh is not checked, so it is a complaint");
        assert_eq!(complaint.check, Check::Docker);
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
        let complaint = docker(Some("unix:///nonexistent-holosmith-test.sock"))
            .expect("a socket that does not exist is not reachable");
        assert_eq!(complaint.check, Check::Docker);
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
        let complaint = docker(Some("/var/run/docker.sock")).expect("a bare path is not a scheme");
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
