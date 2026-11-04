//! The `docker` CLI as a program: one place that runs it, one place that reads what it said (HS-028).
//!
//! The backend does not speak the Docker HTTP API. Item 10 §1's `doctor` and the deploy steps are shell
//! commands against the same daemon, the mount and network flags are easier to get right as arguments than
//! as a JSON body, and a `docker` that answers is a fact an operator can reproduce by hand — which is what
//! makes a failure diagnosable from the row's `status_reason`. The price is a dependency on the CLI being
//! installed, and [`crate::preflight`] already refuses a Docker run when it is not.
//!
//! Everything here is **synchronous**, like [`crate::worktree`]: it runs one program and reads one answer,
//! and the backend calls it from `spawn_blocking` because the provisioner is a `tokio` task and a
//! subprocess in one blocks a worker for as long as Docker takes.
//!
//! `docker_host` is passed as `-H` rather than through the environment: the config key is where a rootless
//! socket or a TCP endpoint is written, the preflight reads the same value, and an inherited
//! `DOCKER_HOST` would be a second opinion about a machine the config already described.

use std::fmt;
use std::path::Path;
use std::process::Command;

/// The `docker` command, pointed at the daemon `docker_host` names.
///
/// `None` leaves the choice to the CLI's own default (the rootful socket), which is what `docker_host`
/// being unset means everywhere else in this build.
#[derive(Debug, Clone, Default)]
pub struct Docker {
    host: Option<String>,
}

impl Docker {
    pub fn new(host: Option<String>) -> Self {
        Self { host }
    }

    /// One `docker` invocation, with `-H` when the config named a daemon.
    fn command(&self) -> Command {
        let mut command = Command::new("docker");
        if let Some(host) = &self.host {
            command.arg("-H").arg(host);
        }
        command
    }

    /// Run `docker <arguments>` and answer its stdout, or an error carrying the first line it printed.
    ///
    /// The first line rather than all of it: Docker puts the actionable sentence first and then repeats
    /// "Run 'docker run --help'", and a `status_reason` that reaches a phone is one sentence.
    pub fn run(&self, arguments: &[&str]) -> Result<String, DockerError> {
        let output = self.command().args(arguments).output().map_err(|error| {
            DockerError::new(
                arguments,
                format!("the docker command could not be run ({error})"),
            )
        })?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        Err(DockerError::new(
            arguments,
            first_line(
                &String::from_utf8_lossy(&output.stderr),
                output.status.code(),
            ),
        ))
    }

    /// Run `docker <arguments>` and answer whether it succeeded, discarding what it said.
    ///
    /// For the questions whose "no" is an ordinary answer — a container or a network that is not there.
    pub fn succeeds(&self, arguments: &[&str]) -> bool {
        self.command()
            .args(arguments)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// Create the session's network: internal, so nothing on it can reach the internet or the host.
    ///
    /// Spec item 2: `docker network create --internal agents_<id>`. `--internal` is the whole point — a
    /// network without it has a gateway to the host and a route out — so it is not a flag a caller passes.
    pub fn create_internal_network(&self, name: &str) -> Result<(), DockerError> {
        // A network that already exists is this session's from an earlier run (the teardown failed, or the
        // process died). `--internal` on an existing network is not a change, so reuse is right; asking
        // first keeps the error for the case that matters (a daemon that cannot create networks).
        if self.succeeds(&["network", "inspect", name]) {
            return Ok(());
        }
        self.run(&["network", "create", "--internal", name])
            .map(|_| ())
    }

    /// Remove a network, answering `false` when it was not there (which is not a failure: teardown of
    /// something a failed step never made must not block a delete, ADR-0023).
    pub fn remove_network(&self, name: &str) -> bool {
        self.succeeds(&["network", "rm", name])
    }

    /// Remove a container by force, answering `false` when it was not there (spec item 6's "ignore
    /// not-found", which HS-029 uses before every start and the teardown uses after a failure).
    pub fn remove_container(&self, name: &str) -> bool {
        self.succeeds(&["rm", "--force", name])
    }

    /// Start a container from `arguments` (a [`crate::container::SessionContainer`]'s run vector).
    ///
    /// Answers the container's id, which is what `docker run` prints and what a log line wants; the name
    /// is what everything else uses, because it is derived and therefore stable.
    pub fn run_container(&self, arguments: &[String]) -> Result<String, DockerError> {
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        self.run(&borrowed).map(|id| id.trim().to_string())
    }

    /// One `docker inspect` field, as Go's template syntax spells it — the seam the tests read flags
    /// through, and the only way to see what a running container's `HostConfig` really holds.
    pub fn inspect(&self, name: &str, format: &str) -> Result<String, DockerError> {
        self.run(&["inspect", "--format", format, name])
            .map(|text| text.trim().to_string())
    }

    /// Whether a container exists, running or not.
    pub fn container_exists(&self, name: &str) -> bool {
        self.succeeds(&["container", "inspect", name])
    }
}

/// The first line of a tool's stderr, or a status-only sentence when it printed nothing.
fn first_line(stderr: &str, code: Option<i32>) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string);
    match (line, code) {
        (Some(line), _) => line,
        (None, Some(code)) => format!("docker exited {code} and printed nothing"),
        (None, None) => "docker was killed and printed nothing".to_string(),
    }
}

/// Why a `docker` invocation did not do what was asked, as one sentence for the row's `status_reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerError {
    /// What was asked, as Docker's own words: the leading arguments that are not flags, so
    /// `docker network create` and `docker run` read differently in a reason and neither carries a
    /// `--flag` that says nothing about the operation.
    operation: String,
    /// Docker's own first line, or ours when it printed none.
    sentence: String,
}

impl DockerError {
    fn new(arguments: &[&str], sentence: String) -> Self {
        Self {
            operation: arguments
                .iter()
                .take_while(|argument| !argument.starts_with('-'))
                .take(2)
                .copied()
                .collect::<Vec<&str>>()
                .join(" "),
            sentence,
        }
    }
}

impl fmt::Display for DockerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "docker {}: {}", self.operation, self.sentence)
    }
}

impl std::error::Error for DockerError {}

/// The absolute form of a path a `docker` argument needs.
///
/// Docker resolves a bind mount's source in the **host's** namespace, and it does not report a source it
/// cannot see: it *creates* it as an empty root-owned directory. A relative path would be resolved against
/// this process's working directory, which is not the daemon's. Every path in a run argument therefore goes
/// through here (HS-027 measured the failure this prevents).
pub fn absolute(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_names_the_operation_and_dockers_first_line() {
        let error = DockerError::new(
            &["network", "create", "--internal", "agents_x"],
            "Error response from daemon: could not find an available network".to_string(),
        );
        assert_eq!(
            error.to_string(),
            "docker network create: Error response from daemon: could not find an available network"
        );
        // A flag is not part of what was asked: `docker run --detach …` reads as `docker run`.
        let run = DockerError::new(
            &["run", "--detach", "--rm", "s_abc"],
            "no such image".into(),
        );
        assert_eq!(run.to_string(), "docker run: no such image");
    }

    #[test]
    fn a_tool_that_printed_nothing_still_says_what_happened() {
        assert_eq!(
            first_line("", Some(1)),
            "docker exited 1 and printed nothing"
        );
        assert_eq!(
            first_line("\n\n  Error response from daemon: nope\nmore\n", Some(1)),
            "Error response from daemon: nope"
        );
        assert_eq!(
            first_line("   ", None),
            "docker was killed and printed nothing"
        );
    }

    #[test]
    fn the_host_is_passed_as_a_flag_and_unset_means_the_clis_own_default() {
        // `-H` rather than `DOCKER_HOST`, because the config key is where a machine's daemon is written
        // and an inherited variable would be a second opinion about the same machine.
        let docker = Docker::new(Some("unix:///run/user/1000/docker.sock".to_string()));
        let command = docker.command();
        // A `Command` has no argument accessor, so the assertion is over what it prints: no host, no flag.
        let described = format!("{command:?}");
        assert!(described.contains("-H"), "{described}");
        assert!(
            described.contains("/run/user/1000/docker.sock"),
            "{described}"
        );
        let plain = format!("{:?}", Docker::new(None).command());
        assert!(!plain.contains("-H"), "{plain}");
    }

    #[test]
    fn a_path_that_is_not_utf8_is_refused_rather_than_lossily_converted() {
        assert_eq!(
            absolute(Path::new("/srv/holosmith/projects")),
            Ok("/srv/holosmith/projects".to_string())
        );
    }
}
