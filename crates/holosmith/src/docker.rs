//! The `docker` CLI as a program: one place that runs it, one place that reads what it said.
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
use std::process::Command;

/// How long a stopped container has before it is killed: spec item 6's `docker stop -t 10`.
pub const STOP_SECONDS: &str = "10";

/// The machine's own network for the proxies that are allowed to reach out (spec item 2).
///
/// Not internal, and not per session: the egress proxy is attached here *and* to each session network, so that
/// a container's only route out is through something this build controls. It is created at startup on a Docker
/// run so the proxies that arrive next have one place to attach to, and its absence is a `warn` rather
/// than a refusal — a session's own network step already fails with a message about itself.
pub const EGRESS_NETWORK: &str = "egress";

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

    /// One `docker` invocation as a value another module can spawn, with `-H` and the arguments set.
    ///
    /// For the one call in this build that is not "run it and read the answer": the agent's launch is
    /// attached, so its stdio belongs to the caller for as long as the container lives. Everything
    /// else here stays synchronous and hands back a string; this hands back the program — which is why there
    /// is no `run_container` beside `stop_container` any more.
    pub fn command_line(&self, arguments: &[String]) -> Command {
        let mut command = self.command();
        command.args(arguments);
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
        self.ensure_network(name, true)
    }

    /// Create a network, reusing one that is already there.
    ///
    /// **An existing network is reused, not recreated**, whatever its flags: it is this session's from an
    /// earlier run (the teardown failed, or the process died) or, for [`EGRESS_NETWORK`], the machine's own. A
    /// network that exists with the wrong flags is a machine problem no `create` can fix, and `network create`
    /// would answer "already exists" — so asking first keeps the error for the case that matters, a daemon
    /// that cannot create networks at all. `internal` is the whole difference between a session network and
    /// the egress one, which is why it is a parameter and not two functions.
    pub fn ensure_network(&self, name: &str, internal: bool) -> Result<(), DockerError> {
        if self.succeeds(&["network", "inspect", name]) {
            return Ok(());
        }
        let mut arguments = vec!["network", "create"];
        if internal {
            arguments.push("--internal");
        }
        arguments.push(name);
        self.run(&arguments).map(|_| ())
    }

    /// The gateway address a network reports, when it has one.
    ///
    /// Only the network self-test asks, and only to learn *which* address the host is reachable on
    /// from a normal network. `None` for a network that does not exist, has no IPAM config (a driver that does
    /// not do IPAM) or reports Docker's `<no value>` for a missing field — three ways of saying "there is no
    /// address here to probe", and none of them is an error.
    ///
    /// **An internal network is not one of those cases**: it reports a gateway address like any other and
    /// simply has no route to it (measured, ADR-0044), which is why nothing here tries to tell internal from
    /// external by reading fields.
    pub fn gateway(&self, network: &str) -> Option<String> {
        let gateway = self
            .inspect(network, "{{(index .IPAM.Config 0).Gateway}}")
            .ok()?;
        if gateway.is_empty() || gateway == "<no value>" {
            return None;
        }
        Some(gateway)
    }

    /// Run `docker <arguments>` under a **bound**, answering `Ok(None)` when the bound expired and the child was
    /// killed.
    ///
    /// The one primitive every startup-path call needs: a daemon that accepts a request and then wedges must not
    /// hang the boot before the listener binds. It is a separate function from [`Docker::run`] because the two
    /// want different things from the same CLI — `run` waits forever and answers a string, and the callers here
    /// answer "it did not finish" rather than a sentence about what Docker said.
    fn bounded(
        &self,
        arguments: &[&str],
        bound: std::time::Duration,
    ) -> Result<Option<std::process::Output>, DockerError> {
        let mut child = self
            .command()
            .args(arguments)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| {
                DockerError::new(
                    arguments,
                    format!("the docker command could not be run ({error})"),
                )
            })?;
        let deadline = std::time::Instant::now() + bound;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(error) => {
                    return Err(DockerError::new(
                        arguments,
                        format!("the docker command could not be waited on ({error})"),
                    ));
                }
            }
        }
        let output = child.wait_with_output().map_err(|error| {
            DockerError::new(
                arguments,
                format!("the docker command printed nothing readable ({error})"),
            )
        })?;
        Ok(Some(output))
    }

    /// Run a **detached** container under a bound, answering its container id.
    ///
    /// The egress proxy's start is the caller: `--detach` means the CLI returns as soon as the daemon has
    /// created the container, and `--restart unless-stopped` means its lifetime is the daemon's, not this
    /// process's. It is **bounded** because it runs on the startup path — where a `docker run` can also have to
    /// pull the image first — and `Ok(None)` is "the start did not finish", not a verdict about the machine.
    pub fn run_detached(
        &self,
        arguments: &[String],
        bound: std::time::Duration,
    ) -> Result<Option<String>, DockerError> {
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let Some(output) = self.bounded(&borrowed, bound)? else {
            return Ok(None);
        };
        if !output.status.success() {
            return Err(DockerError::new(
                &borrowed,
                first_line(
                    &String::from_utf8_lossy(&output.stderr),
                    output.status.code(),
                ),
            ));
        }
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    }

    /// Remove a container by force under a bound, answering whether it happened — the startup path's spelling of
    /// [`Docker::remove_container`], and the same question (spec item 6's "ignore not-found").
    pub fn remove_container_bounded(&self, name: &str, bound: std::time::Duration) -> bool {
        self.removed_bounded(&["rm", "--force", name], bound)
    }

    /// Run `docker <arguments>` under a bound and answer the self-test's report: the probe's exit code and what it
    /// printed.
    ///
    /// The probe reports by **exit code**, so this is the one caller that reads the code rather than the text.
    /// `Ok(None)` is the bound expiring and the child being killed — "the probe did not finish", which is not a
    /// verdict about the machine.
    pub fn attempt(
        &self,
        arguments: &[String],
        bound: std::time::Duration,
    ) -> Result<Option<crate::selftest::Report>, DockerError> {
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let Some(output) = self.bounded(&borrowed, bound)? else {
            return Ok(None);
        };
        Ok(Some(crate::selftest::Report {
            // A process killed by a signal has no code of its own; -1 is "not success and not a number Docker
            // gave us", which the self-test reads as a probe that did not finish.
            code: output.status.code().unwrap_or(-1),
            reached: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            complaint: String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<&str>>()
                .into_iter()
                .rev()
                .collect::<Vec<&str>>()
                .join(" | "),
        }))
    }

    /// Remove a network, answering `false` when it was not there (which is not a failure: teardown of
    /// something a failed step never made must not block a delete, ADR-0023).
    pub fn remove_network(&self, name: &str) -> bool {
        self.succeeds(&["network", "rm", name])
    }

    /// Remove a network under a bound, answering whether it happened — the startup reconcile's spelling of
    /// [`Docker::remove_network`], for [`Docker::remove_container_bounded`]'s reason: it runs before
    /// the listener has an answer for anybody, so a wedged daemon may cost the bound and not the boot.
    pub fn remove_network_bounded(&self, name: &str, bound: std::time::Duration) -> bool {
        self.removed_bounded(&["network", "rm", name], bound)
    }

    /// One bounded removal: `Ok` and a zero exit is the only `true`, and a bound that expired is not a removal
    /// either. The primitive the two above are, so "did it happen" is one expression rather than two.
    fn removed_bounded(&self, arguments: &[&str], bound: std::time::Duration) -> bool {
        self.bounded(arguments, bound)
            .is_ok_and(|output| output.is_some_and(|output| output.status.success()))
    }

    /// Every container the daemon knows, by name, as the startup reconcile reads them (spec item 6).
    ///
    /// The filter is the spec's own (`docker ps -a --filter name=s_`), and it is **not** the rule that decides
    /// what is the build's: Docker's name filter is a substring match, so `name=s_` also answers a container
    /// called `hs_s_things`. The naming convention is checked in Rust ([`crate::reconcile`]), which is why
    /// this answers the daemon's answer and nothing more.
    pub fn container_names(
        &self,
        filter: &str,
        bound: std::time::Duration,
    ) -> Result<Vec<String>, DockerError> {
        let by_name = format!("name={filter}");
        self.names(
            &["ps", "-a", "--filter", &by_name, "--format", "{{.Names}}"],
            bound,
        )
    }

    /// Every network the daemon knows, by name, for the same reason as [`Docker::container_names`].
    pub fn network_names(
        &self,
        filter: &str,
        bound: std::time::Duration,
    ) -> Result<Vec<String>, DockerError> {
        let by_name = format!("name={filter}");
        self.names(
            &[
                "network",
                "ls",
                "--filter",
                &by_name,
                "--format",
                "{{.Name}}",
            ],
            bound,
        )
    }

    /// One listing under a bound: `--format` answers one name per line, so nothing Docker *says* is read
    /// (ADR-0035's rule — a template asks for the field rather than parsing a sentence).
    fn names(
        &self,
        arguments: &[&str],
        bound: std::time::Duration,
    ) -> Result<Vec<String>, DockerError> {
        let Some(output) = self.bounded(arguments, bound)? else {
            return Err(DockerError::new(
                arguments,
                format!("docker did not finish within {}s", bound.as_secs()),
            ));
        };
        if !output.status.success() {
            return Err(DockerError::new(
                arguments,
                first_line(
                    &String::from_utf8_lossy(&output.stderr),
                    output.status.code(),
                ),
            ));
        }
        Ok(listed_names(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Join `container` to `network` under `alias`, replacing any connection it already had.
    ///
    /// The egress proxy is what a session's container can reach, and it reaches it by the **alias** rather
    /// than by the container's name: `docker network connect --alias` is what puts `egress-proxy` in the
    /// session network's DNS, and the proxy is joined to a network of its own as well, so the alias is the only
    /// spelling that means the same thing from both sides (spec item 2).
    ///
    /// **A disconnect first**, so this is idempotent without reading Docker's English: a second `connect`
    /// answers `endpoint with name … already exists in network`, and a resume runs this again over a network
    /// that survived an earlier stop. Forcing the disconnect first also re-applies the alias when it changed.
    pub fn join_network(
        &self,
        network: &str,
        container: &str,
        alias: &str,
    ) -> Result<(), DockerError> {
        let _ = self.succeeds(&["network", "disconnect", "--force", network, container]);
        self.run(&["network", "connect", "--alias", alias, network, container])
            .map(|_| ())
    }

    /// Detach `container` from `network`, answering `false` when it was not attached.
    ///
    /// Load-bearing rather than tidy: `docker network rm` refuses a network with an active endpoint, and a
    /// session's network has one for as long as the egress proxy is joined to it. ADR-0023's rule is
    /// that a delete leaves no `deleting` row, so the teardown leaves the network before it removes it.
    pub fn leave_network(&self, network: &str, container: &str) -> bool {
        self.succeeds(&["network", "disconnect", "--force", network, container])
    }

    /// Whether a container is *running* (not merely present), which is the question a proxy asks about itself.
    pub fn running(&self, name: &str) -> bool {
        self.inspect(name, "{{.State.Running}}")
            .is_ok_and(|running| running == "true")
    }

    /// Remove a container by force, answering `false` when it was not there (spec item 6's "ignore
    /// not-found", which uses before every start and the teardown uses after a failure).
    pub fn remove_container(&self, name: &str) -> bool {
        self.succeeds(&["rm", "--force", name])
    }

    /// Stop a running container, waiting spec item 6's ten seconds before it is killed.
    ///
    /// Answers `Ok(false)` when there was no container, which is an ordinary answer: a session that was never
    /// started, or one whose `--rm` container has already gone, is not a failure (ADR-0023's rule, applied to
    /// the stop policy). It answers `Err` when the daemon could not do it — a wedged daemon and a missing
    /// container are different facts, and a forced abort that leaves a container running must not be
    /// reported as a stop of a container that was never there. The existence check is what tells them apart
    /// without parsing Docker's English.
    ///
    /// The flag that says this stop was expected is written by the caller — `stop::Stopper`, before this
    /// call — because it is a row's column and a backend may not write one; the ten seconds are the
    /// spec's.
    pub fn stop_container(&self, name: &str) -> Result<bool, DockerError> {
        if !self.container_exists(name) {
            return Ok(false);
        }
        self.run(&["stop", "--time", STOP_SECONDS, name])
            .map(|_| true)
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

    /// Whether an image is there, which is a question whose "no" is an ordinary answer.
    ///
    /// Asked before a label read and by the proxies' start: an absent image is "not built yet", and only a
    /// daemon that cannot answer at all is an error. `container_exists`'s shape, one object over.
    pub fn image_exists(&self, image: &str) -> bool {
        self.succeeds(&["image", "inspect", image])
    }

    /// One `docker image inspect --format` field, as Go's template syntax spells it.
    ///
    /// `inspect` rather than `container inspect`, because the subject is an image: the labels a build wrote are
    /// `Config.Labels`, and reading them is how a deploy decides an image is current.
    pub fn image_inspect(&self, image: &str, format: &str) -> Result<String, DockerError> {
        self.run(&["image", "inspect", "--format", format, image])
            .map(|text| text.trim().to_string())
    }

    /// Run one `docker build` with `archive` on its stdin under a bound, answering its combined output.
    pub(crate) fn build_bounded(
        &self,
        arguments: &[String],
        archive: &[u8],
        bound: std::time::Duration,
    ) -> Result<Option<crate::images::Log>, DockerError> {
        self.run_captured(arguments, Some(archive), bound)
    }

    /// Run one `docker` child under a bound with its **output captured in a file** and, when it needs one, its input
    /// written to a pipe from a thread of its own.
    ///
    /// Three things make a build — an image build or a release build — unlike every other call here, and each is why
    /// this is not `run`:
    ///
    /// - **Its input may be a tar on a pipe.** The context is written by a **spawned thread**, so a daemon that
    ///   stops reading stdin cannot hold this call past its deadline: the poll below kills the child and the writer
    ///   dies on a closed pipe. A child that takes no input gets `/dev/null`, because a container that inherits
    ///   this process's stdin could consume a caller's pipe.
    /// - **Its output is hundreds or thousands of lines.** stdout and stderr go to a **temporary file**, not to
    ///   pipes: a pipe nobody drains fills at 64 KiB and the build deadlocks, and a `Command::output` that buffered
    ///   them would hand back the same thing only after the build was over anyway.
    /// - **A failed build is an answer, not an error.** So the status and the text come back together as a
    ///   [`crate::images::Log`], and `Ok(None)` means only "the build did not finish".
    pub(crate) fn run_captured(
        &self,
        arguments: &[String],
        input: Option<&[u8]>,
        bound: std::time::Duration,
    ) -> Result<Option<crate::images::Log>, DockerError> {
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let path = capture_path();
        let capture = std::fs::File::create(&path).map_err(|error| {
            DockerError::new(
                &borrowed,
                format!(
                    "the build's output could not be captured in {} ({error}): a build prints more than a \
                     pipe nobody drains can hold, so its output goes to a file this call reads back",
                    path.display()
                ),
            )
        })?;
        let stderr = capture.try_clone().map_err(|error| {
            DockerError::new(
                &borrowed,
                format!("the build's output file could not be shared ({error})"),
            )
        })?;
        let stdin = match input {
            Some(_) => std::process::Stdio::piped(),
            None => std::process::Stdio::null(),
        };
        let mut child = self
            .command()
            .args(&borrowed)
            .stdin(stdin)
            .stdout(std::process::Stdio::from(capture))
            .stderr(std::process::Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                DockerError::new(
                    &borrowed,
                    format!("the docker command could not be run ({error})"),
                )
            })?;
        let writer = match (input, child.stdin.take()) {
            (None, _) => None,
            (Some(_), None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DockerError::new(
                    &borrowed,
                    "the build's stdin could not be written to, so no context was sent".to_string(),
                ));
            }
            (Some(context), Some(mut stdin)) => {
                // The writer is a thread of its own so that the deadline below runs whatever the daemon does with
                // the context: writing 40 MiB down a pipe the daemon never reads is a `write` that never returns.
                let context = context.to_vec();
                Some(std::thread::spawn(move || {
                    use std::io::Write as _;
                    let _ = stdin.write_all(&context);
                }))
            }
        };
        let deadline = std::time::Instant::now() + bound;
        let finished = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                Err(error) => {
                    return Err(DockerError::new(
                        &borrowed,
                        format!("the docker command could not be waited on ({error})"),
                    ));
                }
            }
        };
        // The child is gone either way, so the writer's pipe is closed and this cannot block on it.
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        let text = std::fs::read(&path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        let Some(status) = finished else {
            return Ok(None);
        };
        Ok(Some(crate::images::Log {
            success: status.success(),
            text,
        }))
    }
}

/// The names one `--format` listing printed: one per line, trimmed, empties dropped.
///
/// Split here rather than by each caller because a listing is the only place Docker answers a *list* of
/// things, and the newline that ends the last line is not a name.
fn listed_names(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
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

/// A fresh path for one build's captured output, unique to this process and this run.
///
/// In the temp directory rather than beside the artifact: a build's output is hundreds of lines nobody keeps,
/// and the path is removed by the call that wrote it whatever the build did.
fn capture_path() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "holosmith-build-{}-{nanos}.log",
        std::process::id()
    ))
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
    fn a_listing_is_one_name_per_line_and_a_blank_line_is_not_one() {
        assert_eq!(
            listed_names("s_0123456789abcdef\nagents_0123456789abcdef\n\n  hs_egress  \n"),
            vec![
                "s_0123456789abcdef".to_string(),
                "agents_0123456789abcdef".to_string(),
                "hs_egress".to_string()
            ]
        );
        assert!(listed_names("").is_empty());
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
}
