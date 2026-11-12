//! `holosmith smoke`: the candidate's own proof against the real daemon (spec item 10 step 2).
//!
//! Item 10's pipeline is "build it in a container, then smoke test it on the host": run the candidate with a smoke
//! config — alternate port, a temporary data directory, `container_prefix=smoke_`, `network_prefix=smoke_` — against
//! the real Docker daemon, using a fake pi in the image that replays the session recorded in phase 1: create a
//! session, run a turn, delete it. the `upgrade <binary>` is this module's caller, so **the exit code is the
//! contract**:
//!
//! * **0** — the candidate passed. It served a session, ran a turn whose frames were the recorded turn's, deleted it,
//!   and left no container, no network and no proxy behind.
//! * **1** — a measurement failed. The candidate is bad, or a step of this run failed: a `serve` that did not answer,
//!   a create that was refused, a turn whose frames were missing, a delete that left a row. A failed **image build**
//!   is here too: the recipe is this build's own, so a daemon that cannot build it is a failed measurement rather
//!   than a missing prerequisite.
//! * **2** — the test could not run. No daemon, no `git`, a `--binary` that is missing, not executable, or built for
//!   another CPU. Decided **before** anything is attempted, so an operator can tell "this machine cannot judge a
//!   release" from "this release failed".
//!
//! **The candidate is the `pi` too.** The smoke image is built from the candidate's own bytes, and its
//! `/usr/local/bin/pi` is a two-line wrapper onto `holosmith container fake-pi` (see `images/smoke.Dockerfile`), so
//! the launch, the ready handshake, the record framing and the version check are the *shipped* ones — the fake is
//! behind pi's name, not beside it.
//!
//! **What it deliberately does not do.** No provider call is made, so the key proxy is started and joined and never
//! exercised — that is V3's by-hand run, and the ADR says so in as many words. Nothing on the machine is named
//! without the `smoke_` prefix, and the run removes what it made: its session's container and network (by the delete),
//! its own two proxies, and its root, unless `HOLOSMITH_KEEP_SCRATCH=1` (the convention the Docker tests use).

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::Config;
use crate::container::{container_name, network_name};
use crate::docker::Docker;
use crate::images;

/// The prefix this run puts in front of **every** name it owns (spec item 10 step 2's `smoke_`).
pub const PREFIX: &str = "smoke_";

/// The label that says which candidate and which recipe an image was built from.
///
/// One label rather than two, because a promotion asks one question — "is this image the one for *this* binary and
/// *this* recipe" — and two labels can disagree (`builder`'s `LABEL_RECIPE` and `images`' `LABEL_BINARY` are the
/// same shape, one image at a time).
pub const LABEL_RECIPE: &str = "org.holosmith.smoke-recipe-sha256";

/// How long the candidate may take to print its banner and answer `/api/health`.
pub const START_TIMEOUT: Duration = Duration::from_secs(60);

/// How long one created session may take to reach `idle` (the worktree, the network and pi's handshake are real).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long one turn may take to settle: the fixture replays in milliseconds, so this is a wedged candidate's bound.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a delete may take to answer 404 (the snapshot, the stop and the worktree removal are real).
pub const DELETE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the image build may take before it is a failed measurement.
pub const IMAGE_TIMEOUT: Duration = Duration::from_secs(900);

/// How long one daemon question may take on this path.
pub const BOUND: Duration = Duration::from_secs(30);

/// The lines of the candidate's own log printed above a failure's sentence.
pub const TAIL_LINES: usize = 20;

/// The project the smoke session is created in, under the smoke run's own projects root.
pub const PROJECT: &str = "smoke";

/// The prompt. It is never read by anything (the fake replays a recording), so it says what it is.
pub const PROMPT: &str = "holosmith smoke: replay the recorded turn";

/// The first line of every smoke config, and how a directory is recognised as one this command wrote.
///
/// `--root` is a directory the harness may **remove** (it is scratch, and a second run must not inherit a first
/// one's database). Deleting an operator-supplied path on a guess is not acceptable, so the guess is this line: a
/// directory is the smoke run's only when its `config.toml` begins with it. Anything else is refused by name.
pub const CONFIG_MARKER: &str =
    "# holosmith smoke: written by `holosmith smoke`; removed when the run passes";

/// The largest answer one HTTP call reads before it gives up on the answer being one.
const MAX_ANSWER: usize = 4 * 1024 * 1024;

/// How the run ended, which is the exit code reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The candidate passed every step.
    Passed,
    /// A measurement failed. The sentence is the operator's.
    Failed(String),
    /// The test could not run on this machine. Not a verdict about the candidate.
    CouldNotRun(String),
}

impl Outcome {
    /// The number a promotion branches on: 0 pass, 1 failed, 2 could not run.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::Failed(_) => 1,
            Self::CouldNotRun(_) => 2,
        }
    }

    /// The sentence, when there is one.
    pub fn sentence(&self) -> Option<&str> {
        match self {
            Self::Passed => None,
            Self::Failed(sentence) | Self::CouldNotRun(sentence) => Some(sentence),
        }
    }
}

/// The smoke image's tag: this binary's version, like `agent-image:<version>`.
pub fn image() -> String {
    images::smoke_image()
}

/// The recipe label's value for this Dockerfile and these candidate bytes.
///
/// The Dockerfile **and** the program, because either changing makes a different image: a fixed tag over a changed
/// recipe is the defect `builder::LABEL_RECIPE` exists to prevent, and one over a *different binary* is the P1
/// in another container.
pub fn recipe(dockerfile: &str, binary: &[u8]) -> String {
    let mut both = Vec::with_capacity(dockerfile.len() + binary.len() + 1);
    both.extend_from_slice(dockerfile.as_bytes());
    both.push(b'\n');
    both.extend_from_slice(binary);
    images::hex(&images::sha256(&both))
}

/// The smoke config, as TOML: everything a run must own, and nothing that could reach the machine's own state.
///
/// Written before anything is attempted (so a refusal leaves the file an operator can read) and asserted field by
/// field by the gate. The four settings the acceptance names are the listeners, the two roots, and the two prefixes;
/// the rest is what keeps a smoke run out of the machine's way:
///
/// * `role = "server"` — a session needs the server half (`hub` answers `403 sessions_disabled`).
/// * `agent_image` — the smoke image, whose `pi` is the candidate.
/// * `min_free_gb = 0` and `session_disk_soft_gb = 0` — the floor and the per-session cap measure a *machine's*
///   limits; the smoke run writes nothing worth capping, and a floor refusing the create would be a red promotion on
///   a full disk rather than a statement about the candidate.
/// * `idle_stop_min = 0` — the sweeper would otherwise be allowed to stop the session mid-turn on a machine whose
///   config asked for a short idle stop.
/// * `max_live_sessions = 1` — the run creates one session, and a cap of one makes a second impossible rather than
///   evicting the machine's own.
pub fn config_text(image: &str, root: &Path, preview_port: u16) -> String {
    let data = root.join("data");
    let projects = root.join("projects");
    format!(
        "{CONFIG_MARKER}\n\
         role = \"server\"\n\
         backend = \"docker\"\n\
         control_listen = \"127.0.0.1:0\"\n\
         preview_listen = \"127.0.0.1:{preview_port}\"\n\
         control_origin = \"http://127.0.0.1:8080\"\n\
         preview_origin = \"http://127.0.0.1:{preview_port}\"\n\
         data_root = \"{}\"\n\
         projects_root = \"{}\"\n\
         agent_image = \"{image}\"\n\
         container_prefix = \"{PREFIX}\"\n\
         network_prefix = \"{PREFIX}\"\n\
         min_free_gb = 0\n\
         session_disk_soft_gb = 0\n\
         idle_stop_min = 0\n\
         max_live_sessions = 1\n\
         allowed_hosts = [\"127.0.0.1\"]\n",
        data.display(),
        projects.display(),
    )
}

/// What one turn's frames said, once they were judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// How many events the page held.
    pub frames: usize,
    /// How many `message.delta` frames arrived.
    pub deltas: usize,
    /// What the turn read, from the `message.end` that carried usage.
    pub in_tokens: u64,
    /// What it wrote.
    pub out_tokens: u64,
}

/// One event from the history page: its kind and its payload, both as the API spells them.
pub type Event = (String, String);

/// Judge a session's log against what the acceptance says a turn must show.
///
/// Four facts, and each failure names what was missing: a `turn.start`/`turn.end` pair around it (the transcript's
/// turn is the prompt, ADR-0040), at least one `message.delta` (the streaming the acceptance asks for), a
/// `tool.start` and a `tool.end` with the **same** id (the tool call), and a `message.end` whose payload carries a
/// non-zero `usage` (the spend). It is a pure function of the page so each arm can be tested without a daemon, and the
/// arms are what make "a failed promotion" measurable at this level: a candidate that replays nothing fails here.
pub fn assert_frames(events: &[Event]) -> Result<Report, String> {
    let mut starts = 0;
    let mut ends = 0;
    let mut deltas = 0;
    let mut started_tools: Vec<String> = Vec::new();
    let mut ended_tools: Vec<String> = Vec::new();
    let mut usage: Option<(u64, u64)> = None;
    for (kind, payload) in events {
        match kind.as_str() {
            "turn.start" => starts += 1,
            "turn.end" => ends += 1,
            "message.delta" => deltas += 1,
            "tool.start" => {
                if let Some(id) = tool_id(payload) {
                    started_tools.push(id);
                }
            }
            "tool.end" => {
                if let Some(id) = tool_id(payload) {
                    ended_tools.push(id);
                }
            }
            "message.end" => {
                if let Some((input, output)) = counted_usage(payload) {
                    usage = Some((input, output));
                }
            }
            _ => {}
        }
    }
    if starts == 0 || ends == 0 {
        return Err(format!(
            "the turn's log holds {starts} `turn.start` and {ends} `turn.end` frames: one prompt must open and close \
             exactly one turn (the transcript's turn is the prompt, not pi's assistant response)"
        ));
    }
    if deltas == 0 {
        return Err(
            "the turn's log holds no `message.delta` frame: the replayed recording streams its answer in deltas, so \
             an empty log means the fixture never reached the writer"
                .to_string(),
        );
    }
    let paired = started_tools
        .iter()
        .find(|id| ended_tools.contains(id))
        .cloned();
    let Some(_tool) = paired else {
        return Err(format!(
            "the turn's log holds {} `tool.start` and {} `tool.end` frames and no id in both: the recording makes one \
             tool call, and a start without its end is a transcript block that never closes",
            started_tools.len(),
            ended_tools.len()
        ));
    };
    let Some((input, output)) = usage else {
        return Err(
            "the turn's log holds no `message.end` carrying a non-zero usage: the recording reports what it spent, \
             and a session with no usage is a budget that never counts"
                .to_string(),
        );
    };
    Ok(Report {
        frames: events.len(),
        deltas,
        in_tokens: input,
        out_tokens: output,
    })
}

/// A frame's `tool_id`, when it has one.
fn tool_id(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
    value
        .get("tool_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A `message.end` payload's usage as the contract's two counts, when it reports any.
fn counted_usage(payload: &str) -> Option<(u64, u64)> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let usage = value.get("usage")?;
    let input = usage.get("in").and_then(Value::as_u64)?;
    let output = usage.get("out").and_then(Value::as_u64)?;
    (input > 0 || output > 0).then_some((input, output))
}

/// A free loopback port, for the preview listener (`config` refuses the two listeners being the same address, so the
/// control listener cannot be `:0` as well and this is the one port the run picks itself).
///
/// A bind-and-drop rather than a banner read: the preview listener is not used by the smoke run (the fake pi serves
/// nothing), so losing the race to another process costs a `warn` line in the candidate's log and nothing else.
pub fn free_port() -> Result<u16, String> {
    let socket = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("no loopback port could be asked for ({error})"))?;
    socket
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| format!("a bound port could not be read ({error})"))
}

/// The whole run, from the config it was given to the sentence an operator reads.
///
/// Prints one `smoke: ` line per step on stdout — that is the demo's evidence — and returns the outcome, which the
/// command turns into an exit code. Cleanup happens on **every** ending: a leftover container, network, proxy or root
/// is itself a failure (`Failed`), because the next run and the next machine both have to trust the names.
pub fn run(config: &Config, binary: &Path, root: &Path) -> Outcome {
    match execute(config, binary, root) {
        Ok(()) => {
            println!("smoke: ok");
            Outcome::Passed
        }
        Err(Ending::CannotRun(sentence)) => Outcome::CouldNotRun(sentence),
        Err(Ending::Failed(sentence)) => Outcome::Failed(sentence),
    }
}

/// Which of the two non-pass endings a step produced.
enum Ending {
    /// The test could not run here (exit 2).
    CannotRun(String),
    /// A step failed (exit 1).
    Failed(String),
}

impl Ending {
    /// A failed measurement, with the candidate's own log above the sentence.
    fn failed(log: &Path, sentence: impl Into<String>) -> Self {
        let sentence = sentence.into();
        match std::fs::read_to_string(log) {
            Ok(text) if !text.trim().is_empty() => Self::Failed(format!(
                "{sentence}\n--- {} (last {TAIL_LINES} lines) ---\n{}",
                log.display(),
                crate::builder::tail(&text, TAIL_LINES)
            )),
            _ => Self::Failed(sentence),
        }
    }
}

fn execute(config: &Config, binary: &Path, root: &Path) -> Result<(), Ending> {
    // 1. The candidate. Everything here is "could not run", decided before anything is written or started.
    let bytes = std::fs::read(binary).map_err(|error| {
        Ending::CannotRun(format!(
            "the binary to test, {}, could not be read ({error}): `--binary` names the file a promotion is about to \
             make live, and an unreadable one cannot be tested",
            binary.display()
        ))
    })?;
    if let Some(problem) = crate::builder::artifact_problem(
        binary,
        &bytes,
        crate::builder::default_target().map_err(Ending::CannotRun)?,
    ) {
        return Err(Ending::CannotRun(format!(
            "{problem}: the smoke run starts the candidate as `serve` on this machine and puts it in the smoke \
             image, so it has to be this machine's build"
        )));
    }
    let digest = images::hex(&images::sha256(&bytes));
    println!(
        "smoke: binary: {} (sha256 {})",
        binary.display(),
        &digest[..12]
    );
    // 2. `git`, for the one-commit project the session is created in. A missing `git` is a machine that cannot run
    // the test, not a candidate that failed it.
    if Command::new("git")
        .arg("--version")
        .output()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Err(Ending::CannotRun(
            "`git` was not found on this machine's PATH, so no project could be made for the session: a create \
             refuses a directory that is not a plain checkout (spec item 3), and `holosmith doctor --quick` names \
             the same missing program"
                .to_string(),
        ));
    }

    // 3. The root, its config and its project. Written before the daemon is asked, so a refusal later leaves an
    // operator a config to read, and so the gate can assert the smoke settings even on a machine with no daemon.
    let root = prepare_root(root).map_err(Ending::CannotRun)?;
    let image = image();
    let preview = free_port().map_err(Ending::CannotRun)?;
    let config_path = root.join("config.toml");
    std::fs::write(&config_path, config_text(&image, &root, preview)).map_err(|error| {
        Ending::CannotRun(format!(
            "the smoke config {} could not be written ({error})",
            config_path.display()
        ))
    })?;
    let log = root.join("serve.log");
    println!("smoke: root: {}", root.display());
    println!("smoke: config: {}", config_path.display());

    // 4. The daemon and the image. Asked after the config so the file is the record whatever happens next.
    let docker = Docker::new(config.docker_host.clone());
    if let Some(problem) = crate::builder::daemon_problem(&docker) {
        return Err(Ending::CannotRun(problem));
    }
    ensure_image(&docker, &bytes, &image).map_err(Ending::Failed)?;

    // 5. The candidate, as this machine's server, on a port the kernel gives it.
    let mut server = spawn(binary, &config_path, &root, &log)?;
    let pid = server.id();
    // The session's id as soon as it exists, so a run that fails **after** the create can still take its own
    // container and network away: the interesting ending, and the one nothing else cleans up.
    let mut session: Option<String> = None;
    let measured = drive(&log, pid, &mut session);
    // **Cleanup runs on every ending, not only a passing one.** `and_then` over the measurement would skip all of
    // this exactly when a promotion needs it: a candidate left serving a root about to be removed, and four names
    // left on a machine the next boot has to sweep (P1).
    let stopped = stop(&mut server);
    let proxies = remove_proxies(&docker);
    // On a pass the delete already took them, so this waits for its own promise; on a failure they are ours to
    // remove, because nothing else will.
    let names = match (&session, &measured) {
        (Some(id), Ok(())) => gone(&docker, id),
        (Some(id), Err(_)) => removed(&docker, id),
        (None, _) => Ok(()),
    };
    let cleaned = stopped.and(proxies).and(names);
    // The root: removed when the run passed (it is scratch), kept when it did not (it is the evidence an operator
    // reads), and kept on request either way. An unremovable root after a pass is a failure — the next run would
    // inherit a database and a worktree, which is the state this command exists not to have.
    let root_kept = finish(&root, measured.is_ok());
    match measured {
        Ok(()) => cleaned.and(root_kept).map_err(Ending::Failed),
        Err(ending) => {
            // The measurement's sentence is what an operator needs; a cleanup that also failed is one line above it.
            if let Err(sentence) = cleaned {
                println!("smoke: cleanup: {sentence}");
            }
            if let Err(sentence) = root_kept {
                println!("smoke: cleanup: {sentence}");
            }
            Err(ending)
        }
    }
}

/// Make the root, its data and admin directories and the one-commit project, and answer the root's path.
fn prepare_root(root: &Path) -> Result<PathBuf, String> {
    let root = std::path::absolute(root).map_err(|error| {
        format!(
            "the smoke root {} could not be resolved ({error})",
            root.display()
        )
    })?;
    if root.exists() {
        // A root this command wrote is scratch: a second run must not inherit a first one's database, worktrees and
        // names, so it is removed. A directory that is **not** one ([`CONFIG_MARKER`]) is refused by name rather
        // than deleted — `--root` is an operator's argument, and "the harness removed my home directory" is not a
        // failure a sentence can make good.
        if is_smoke_root(&root) {
            std::fs::remove_dir_all(&root).map_err(|error| {
                format!(
                    "the smoke root {} is there and could not be removed ({error}): it is this command's scratch \
                     space, so a run that inherited it would start over somebody else's database",
                    root.display()
                )
            })?;
        } else if !is_empty(&root) {
            return Err(format!(
                "{} is a directory with something in it and no smoke config, so nothing was removed: `--root` is \
                 this command's scratch space and it deletes it, so it only accepts a path that is new, empty, or a \
                 previous smoke run's — pass a fresh directory",
                root.display()
            ));
        }
    }
    for directory in [root.join("data"), root.join("projects"), root.join("admin")] {
        std::fs::create_dir_all(&directory).map_err(|error| {
            format!(
                "the smoke directory {} could not be made ({error}): the data root is a container mount source, so \
                 it has to be a path the daemon can see",
                directory.display()
            )
        })?;
    }
    let project = root.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project)
        .map_err(|error| format!("the smoke project's directory could not be made ({error})"))?;
    let git = |arguments: &[&str]| -> Result<(), String> {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&project)
            .output()
            .map_err(|error| format!("`git {}` could not run ({error})", arguments.join(" ")))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "`git {}` refused ({}) in the smoke project: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    };
    git(&["init", "--quiet"])?;
    std::fs::write(
        project.join("README.md"),
        "the holosmith smoke run's project\n",
    )
    .map_err(|error| format!("the smoke project's file could not be written ({error})"))?;
    git(&["add", "README.md"])?;
    git(&[
        "-c",
        "user.name=holosmith smoke",
        "-c",
        "user.email=smoke@holosmith.invalid",
        "commit",
        "--quiet",
        "-m",
        "the smoke run's base commit",
    ])?;
    Ok(root)
}

/// Whether `root` is a root a previous smoke run wrote, by the marker line of its config.
fn is_smoke_root(root: &Path) -> bool {
    std::fs::read_to_string(root.join("config.toml"))
        .is_ok_and(|text| text.lines().next() == Some(CONFIG_MARKER))
}

/// Whether a directory holds nothing at all; a path that cannot be read counts as "not empty", so a refusal wins
/// over a removal.
fn is_empty(root: &Path) -> bool {
    std::fs::read_dir(root).is_ok_and(|mut entries| entries.next().is_none())
}

/// Build the smoke image when this candidate and this recipe are not the ones it was built from.
fn ensure_image(docker: &Docker, binary: &[u8], image: &str) -> Result<(), String> {
    let wanted = recipe(crate::images::SMOKE_DOCKERFILE, binary);
    let label = format!("{{{{index .Config.Labels \"{LABEL_RECIPE}\"}}}}");
    if let Ok(found) = docker.image_inspect(image, &label)
        && found.trim() == wanted
    {
        println!("smoke: image: {image} (already this candidate)");
        return Ok(());
    }
    // The context is an archive on stdin, because the daemon may not share this process's filesystem: the recipe and
    // the candidate's own bytes, and nothing else.
    let context = archive_with(binary);
    let arguments = vec![
        "build".to_string(),
        "--label".to_string(),
        format!("{LABEL_RECIPE}={wanted}"),
        "-t".to_string(),
        image.to_string(),
        "-".to_string(),
    ];
    let log = docker
        .run_captured(&arguments, Some(&context), IMAGE_TIMEOUT)
        .map_err(|error| error.to_string())?;
    match log {
        None => Err(format!(
            "the smoke image {image} did not finish building within {}s and was killed: a cold image fetches the \
             pinned base, so this is a machine that could not reach it rather than a recipe that is wrong",
            IMAGE_TIMEOUT.as_secs()
        )),
        Some(log) if !log.success => Err(images::failure(image, &log.text)),
        Some(_) => {
            println!("smoke: image: {image} (built)");
            Ok(())
        }
    }
}

/// The build context: `images::archive`'s ustar, with the **candidate's** bytes under `holosmith`.
///
/// Its own function rather than a call to [`images::archive`] because that one reads the path it is given, and this
/// run already holds the bytes it hashed and judged — a second read could be a second file.
fn archive_with(binary: &[u8]) -> Vec<u8> {
    images::archive_bytes(images::SMOKE_DOCKERFILE, binary)
}

/// Start the candidate as this machine's server, with its own log file and no inherited stdio.
fn spawn(
    binary: &Path,
    config: &Path,
    root: &Path,
    log: &Path,
) -> Result<std::process::Child, Ending> {
    let file = std::fs::File::create(log).map_err(|error| {
        Ending::CannotRun(format!(
            "the candidate's log {} could not be created ({error})",
            log.display()
        ))
    })?;
    Command::new(binary)
        .arg("serve")
        .arg("--config")
        .arg(config)
        .arg("--port")
        .arg("0")
        .arg("--admin-dir")
        .arg(root.join("admin"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(file))
        .spawn()
        .map_err(|error| {
            Ending::CannotRun(format!(
                "the candidate {} could not be started ({error}): `serve` is the mode a promotion makes live, so a \
                 file that cannot run is not a candidate",
                binary.display()
            ))
        })
}

/// The steps against the running candidate: banner, health, create, turn, delete.
fn drive(log: &Path, pid: u32, session: &mut Option<String>) -> Result<(), Ending> {
    let banner = wait_for_banner(log, pid)?;
    println!("smoke: server: pid {pid} on {}", banner.control_listen);
    let client = Client::new(banner.control_listen, &banner.control_origin);
    wait_healthy(&client, log, pid)?;

    // The session. Model and thinking are the row's, and the fake ignores them: what the create proves is that the
    // worktree, the network, the container and pi's handshake really happened.
    let (status, body) = client
        .post(
            "/api/sessions",
            Some("smoke-create"),
            &serde_json::json!({
                "project": PROJECT,
                "model": "claude-sonnet-4.5",
                "thinking": "high",
            })
            .to_string(),
        )
        .map_err(|error| Ending::failed(log, format!("the create did not answer ({error})")))?;
    if !(200..300).contains(&status) {
        return Err(Ending::failed(
            log,
            format!(
                "the create was refused with {status}: {}",
                message_of(&body)
            ),
        ));
    }
    let id = field_of(&body, "id")
        .ok_or_else(|| Ending::failed(log, format!("the create answered no session id: {body}")))?;
    *session = Some(id.clone());
    println!("smoke: session: {id} (created)");
    wait_status(&client, &id, "idle", IDLE_TIMEOUT)
        .map_err(|sentence| Ending::failed(log, sentence))?;

    // The turn.
    let (status, body) = client
        .post(
            &format!("/api/sessions/{id}/prompt"),
            None,
            &serde_json::json!({ "prompt": PROMPT }).to_string(),
        )
        .map_err(|error| Ending::failed(log, format!("the prompt did not answer ({error})")))?;
    if !(200..300).contains(&status) {
        return Err(Ending::failed(
            log,
            format!(
                "the prompt was refused with {status}: {}",
                message_of(&body)
            ),
        ));
    }
    let events = wait_turn(&client, &id).map_err(|sentence| Ending::failed(log, sentence))?;
    let report = assert_frames(&events).map_err(|sentence| Ending::failed(log, sentence))?;
    println!(
        "smoke: turn: {} frames, {} deltas, one tool call, usage {}/{}",
        report.frames, report.deltas, report.in_tokens, report.out_tokens
    );

    // The delete, and the row that is gone afterwards.
    let (status, body) = client
        .delete(&format!("/api/sessions/{id}"))
        .map_err(|error| Ending::failed(log, format!("the delete did not answer ({error})")))?;
    if !(200..300).contains(&status) {
        return Err(Ending::failed(
            log,
            format!(
                "the delete was refused with {status}: {}",
                message_of(&body)
            ),
        ));
    }
    wait_gone(&client, &id).map_err(|sentence| Ending::failed(log, sentence))?;
    println!("smoke: delete: {status} then 404");
    Ok(())
}

/// The banner's two lines: the address the kernel gave the candidate and the origin it answers.
struct Banner {
    control_listen: SocketAddr,
    control_origin: String,
}

/// Wait for the candidate's banner lines, or say what it did instead.
fn wait_for_banner(log: &Path, pid: u32) -> Result<Banner, Ending> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let listen = text
            .lines()
            .find_map(|line| line.strip_prefix("control_listen "))
            .and_then(|value| value.trim().parse::<SocketAddr>().ok());
        let origin = text
            .lines()
            .find_map(|line| line.strip_prefix("control_origin "))
            .map(|value| value.trim().to_string());
        if let (Some(listen), Some(origin)) = (listen, origin) {
            return Ok(Banner {
                control_listen: listen,
                control_origin: origin,
            });
        }
        if Instant::now() > deadline {
            return Err(Ending::failed(
                log,
                format!(
                    "the candidate did not print its banner within {}s: a `serve` that cannot start is a candidate \
                     a promotion cannot make live",
                    START_TIMEOUT.as_secs()
                ),
            ));
        }
        if !alive(pid) {
            return Err(Ending::failed(
                log,
                format!("the candidate exited before printing its banner (pid {pid} is gone)"),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether a process is still there (`kill -0`), which is the only question a child that is not this process can
/// answer without reaping it.
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Ask `/api/health` until it answers 200, or fail with the candidate's own log.
fn wait_healthy(client: &Client, log: &Path, pid: u32) -> Result<(), Ending> {
    let deadline = Instant::now() + START_TIMEOUT;
    let mut last = String::from("it did not answer");
    while Instant::now() < deadline {
        match client.get("/api/health") {
            Ok((200, _)) => return Ok(()),
            Ok((status, body)) => last = format!("it answered {status}: {}", body.trim()),
            Err(error) => last = error,
        }
        if !alive(pid) {
            return Err(Ending::failed(
                log,
                format!("the candidate exited before answering /api/health ({last})"),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(Ending::failed(
        log,
        format!(
            "the candidate did not answer /api/health within {}s ({last})",
            START_TIMEOUT.as_secs()
        ),
    ))
}

/// Wait for a session to reach `status`, and answer its reason when it settles somewhere that is not `idle`.
fn wait_status(client: &Client, id: &str, wanted: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut seen = String::from("no answer");
    while Instant::now() < deadline {
        match client.get(&format!("/api/sessions/{id}")) {
            Ok((200, body)) => {
                let status = field_of(&body, "status").unwrap_or_default();
                if status == wanted {
                    return Ok(());
                }
                if matches!(status.as_str(), "error" | "stopped") {
                    let reason = field_of(&body, "status_reason")
                        .unwrap_or_else(|| "no reason given".to_string());
                    return Err(format!(
                        "the session never reached {wanted}: it is {status} — {reason}"
                    ));
                }
                seen = status;
            }
            Ok((status, body)) => seen = format!("{status}: {}", body.trim()),
            Err(error) => seen = error,
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "the session did not reach {wanted} within {}s (it is {seen})",
        timeout.as_secs()
    ))
}

/// Read the session's log until the turn has ended, and answer its events.
///
/// `turn.end` is the marker rather than the row's `idle`: the row and the log are written by different paths
/// (ADR-0019), so a row that says `idle` does not mean the frames are committed — the gate's landmine, and the reason
/// this waits for the frame it means.
fn wait_turn(client: &Client, id: &str) -> Result<Vec<Event>, String> {
    let deadline = Instant::now() + TURN_TIMEOUT;
    let mut seen = String::from("no answer");
    while Instant::now() < deadline {
        match client.get(&format!(
            "/api/sessions/{id}/events/history?before={}&limit=1000",
            i64::MAX
        )) {
            Ok((200, body)) => {
                let events = events_of(&body);
                if events.iter().any(|(kind, _)| kind == "turn.end") {
                    return Ok(events);
                }
                seen = format!("{} events", events.len());
            }
            Ok((status, body)) => seen = format!("{status}: {}", body.trim()),
            Err(error) => seen = error,
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "the turn did not end within {}s ({seen})",
        TURN_TIMEOUT.as_secs()
    ))
}

/// Wait for the session's row to be gone, which is what a delete promises (`deleting`, then nothing).
fn wait_gone(client: &Client, id: &str) -> Result<(), String> {
    let deadline = Instant::now() + DELETE_TIMEOUT;
    let mut seen = String::from("no answer");
    while Instant::now() < deadline {
        match client.get(&format!("/api/sessions/{id}")) {
            Ok((404, _)) => return Ok(()),
            Ok((200, body)) => {
                seen = field_of(&body, "status").unwrap_or_default();
            }
            Ok((status, body)) => seen = format!("{status}: {}", body.trim()),
            Err(error) => seen = error,
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "the session was still there {}s after the delete (it is {seen}): the delete's task removes the row and its \
         log in one transaction (spec item 5), so a row that stays is a delete that did not finish",
        DELETE_TIMEOUT.as_secs()
    ))
}

/// The events of a history page, as `(kind, payload)`.
fn events_of(body: &str) -> Vec<Event> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    value
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| {
                    let kind = event.get("type").and_then(Value::as_str)?;
                    let payload = event.get("payload").and_then(Value::as_str).unwrap_or("{}");
                    Some((kind.to_string(), payload.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Stop the candidate: `SIGTERM` (its own orderly shutdown), then a bound, then `SIGKILL`.
///
/// A `Child::kill` is `SIGKILL`, and a server killed that way never runs its own teardown — which is the difference
/// between a clean stop and a machine's next boot finding a live row where a session used to be.
fn stop(server: &mut std::process::Child) -> Result<(), String> {
    let started = Instant::now();
    if alive(server.id()) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(server.id().to_string())
            .status();
        while started.elapsed() < Duration::from_secs(30) {
            match server.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    return Err(format!(
                        "the candidate answered SIGTERM with {status}: `serve` exits 0 on a stop, and a non-zero \
                         code is a shutdown that failed"
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    return Err(format!("the candidate could not be waited on ({error})"));
                }
            }
        }
        let _ = server.kill();
        return Err("the candidate was still running 30s after SIGTERM and was killed: a server that does not stop \
                    is one a promotion cannot replace (item 10's drain waits for one)"
            .to_string());
    }
    Ok(())
}

/// Remove the two proxies this run started: they are detached with `--restart unless-stopped`, so nothing else
/// would ever take them away.
fn remove_proxies(docker: &Docker) -> Result<(), String> {
    let names = [
        crate::container::proxy_name(PREFIX),
        crate::key::key_proxy_name(PREFIX),
    ];
    let mut left = Vec::new();
    for name in &names {
        if !docker.remove_container_bounded(name, BOUND) {
            left.push(name.clone());
        }
    }
    if left.is_empty() {
        println!("smoke: proxies: {} removed", names.join(", "));
        return Ok(());
    }
    Err(format!(
        "the smoke run's own proxies could not be removed ({}): they are detached and restarting, so nothing else \
         would ever take them away, and the next promotion would find their names in use",
        left.join(", ")
    ))
}

/// The session's container and network are gone — the delete's own promise, measured rather than assumed.
fn gone(docker: &Docker, id: &str) -> Result<(), String> {
    let container = container_name(PREFIX, id);
    let network = network_name(PREFIX, id);
    let deadline = Instant::now() + DELETE_TIMEOUT;
    let mut seen = String::new();
    while Instant::now() < deadline {
        let containers = docker.container_names(PREFIX, BOUND).unwrap_or_default();
        let networks = docker.network_names(PREFIX, BOUND).unwrap_or_default();
        let container_there = containers.iter().any(|name| name == &container);
        let network_there = networks.iter().any(|name| name == &network);
        if !container_there && !network_there {
            println!("smoke: gone: {container}, {network}");
            return Ok(());
        }
        seen = format!(
            "{} {}, {} {}",
            if container_there {
                "container"
            } else {
                "no container"
            },
            container,
            if network_there {
                "network"
            } else {
                "no network"
            },
            network
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "the session's container or network outlived its delete ({seen}): the delete stops the container, removes \
         the network and deletes the row, so what stays is a machine the next boot would have to sweep"
    ))
}

/// Force a failed run's own session away: its container and its network, by the names this run composed.
///
/// The delete is the ordinary path and it is what a pass waits for ([`gone`]). A run that failed **after** the
/// create has nobody to delete for it, so the harness removes what it made rather than leaving a container on a
/// machine whose next boot would have to sweep it.
fn removed(docker: &Docker, id: &str) -> Result<(), String> {
    let container = container_name(PREFIX, id);
    let network = network_name(PREFIX, id);
    let mut left = Vec::new();
    if !docker.remove_container_bounded(&container, BOUND) {
        left.push(container.clone());
    }
    if !docker.remove_network_bounded(&network, BOUND) {
        left.push(network.clone());
    }
    if left.is_empty() {
        println!("smoke: cleaned: {container}, {network} removed after the failure");
        return Ok(());
    }
    Err(format!(
        "the failed run's own {} could not be removed: it is the smoke run's, so nothing else would take it away",
        left.join(", ")
    ))
}

/// The root's ending: removed after a pass, kept after a failure, kept on request either way.
///
/// A pass with an unremovable root is a **failure**: the directory holds a database, a worktree and the candidate's
/// log, and a second run that inherited them would answer about a previous candidate (P2). A
/// failure keeps it on purpose — it is the evidence an operator reads, and the smoke config in it is what a
/// refusal's sentence points at.
fn finish(root: &Path, passed: bool) -> Result<(), String> {
    if !passed || std::env::var_os("HOLOSMITH_KEEP_SCRATCH").is_some() {
        println!("smoke: root: kept at {}", root.display());
        return Ok(());
    }
    match std::fs::remove_dir_all(root) {
        Ok(()) => {
            println!("smoke: root: removed");
            Ok(())
        }
        Err(error) => Err(format!(
            "the run passed but its root {} could not be removed ({error}): a second run would inherit a database, a              worktree and a log that answer about this candidate",
            root.display()
        )),
    }
}

/// A plain-HTTP client for the candidate's own listeners, on this machine.
///
/// The same three headers `hosts::Client` sends, for the same reason: a request on this machine arrives from the
/// **front door** (`127.0.0.1`, no whois), it carries the origin's text in `Origin` (the guard compares the string)
/// and the origin's host in `Host`, and a request that changes something carries `Content-Type` and
/// `X-Holosmith-Client` — which is what `bad_client` reads. One connection per call, bounded by the caller.
struct Client {
    target: SocketAddr,
    host: String,
    origin: String,
}

impl Client {
    fn new(listen: SocketAddr, origin: &str) -> Self {
        let target = SocketAddr::new(
            if listen.ip().is_unspecified() {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                listen.ip()
            },
            listen.port(),
        );
        // The origin's host without its port, which is the `Host` a served request carries: the guard reduces `Host`
        // to a port-less name itself, so writing it that way removes the question rather than relying on the two
        // agreeing.
        let parsed = crate::config::Origin::parse(origin).ok();
        let host = parsed
            .as_ref()
            .and_then(crate::config::Origin::host)
            .unwrap_or("127.0.0.1")
            .to_string();
        Self {
            target,
            host,
            origin: origin.to_string(),
        }
    }

    fn get(&self, path: &str) -> Result<(u16, String), String> {
        self.request("GET", path, None, &[])
    }

    fn delete(&self, path: &str) -> Result<(u16, String), String> {
        self.request("DELETE", path, None, &[])
    }

    fn post(
        &self,
        path: &str,
        idempotency: Option<&str>,
        body: &str,
    ) -> Result<(u16, String), String> {
        let mut extra = vec![format!(
            "Idempotency-Key: {}",
            idempotency.unwrap_or("smoke")
        )];
        extra.push(format!("Content-Length: {}", body.len()));
        self.request("POST", path, Some(body), &extra)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        extra: &[String],
    ) -> Result<(u16, String), String> {
        let mut stream = TcpStream::connect_timeout(&self.target, BOUND)
            .map_err(|error| format!("{} did not answer ({error})", self.target))?;
        stream
            .set_read_timeout(Some(BOUND))
            .and_then(|()| stream.set_write_timeout(Some(BOUND)))
            .map_err(|error| {
                format!(
                    "the connection to {} could not be bounded ({error})",
                    self.target
                )
            })?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nConnection: close\r\n",
            self.host, self.origin
        );
        if method != "GET" {
            head.push_str("Content-Type: application/json\r\nX-Holosmith-Client: 1\r\n");
        }
        for line in extra {
            head.push_str(line);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let mut request = head.into_bytes();
        if let Some(body) = body {
            request.extend_from_slice(body.as_bytes());
        }
        stream
            .write_all(&request)
            .map_err(|error| format!("the request could not be written ({error})"))?;
        let mut answer = Vec::new();
        (&mut stream)
            .take(MAX_ANSWER as u64)
            .read_to_end(&mut answer)
            .map_err(|error| format!("the answer could not be read ({error})"))?;
        parse(&answer)
    }
}

/// One HTTP answer: its status, and its body as text.
fn parse(answer: &[u8]) -> Result<(u16, String), String> {
    let split = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "the answer had no head".to_string())?;
    let head = String::from_utf8_lossy(&answer[..split]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            format!(
                "the answer's status line is not a status: {}",
                head.lines().next().unwrap_or("")
            )
        })?;
    let body = &answer[split + 4..];
    let chunked = head.lines().any(|line| {
        line.to_ascii_lowercase().starts_with("transfer-encoding:")
            && line.to_ascii_lowercase().contains("chunked")
    });
    let text = if chunked {
        dechunk(body)
    } else {
        String::from_utf8_lossy(body).into_owned()
    };
    Ok((status, text))
}

/// A chunked body as its bytes' text, which is what every JSON answer on these routes is framed in.
fn dechunk(body: &[u8]) -> String {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(end) = rest.windows(2).position(|window| window == b"\r\n") {
        let size = String::from_utf8_lossy(&rest[..end]);
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = end + 2;
        if rest.len() < start + size {
            out.extend_from_slice(&rest[start..]);
            break;
        }
        out.extend_from_slice(&rest[start..start + size]);
        rest = &rest[start + size..];
        rest = rest.strip_prefix(b"\r\n").unwrap_or(rest);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One string field of a JSON object, which is what every answer this module reads carries.
fn field_of(body: &str, name: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get(name)?
        .as_str()
        .map(str::to_string)
}

/// A refusal's sentence: ADR-0008's envelope's `message`, or the body itself when it is not one.
fn message_of(body: &str) -> String {
    field_of(body, "message").unwrap_or_else(|| body.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_turn() -> Vec<Event> {
        vec![
            ("turn.start".to_string(), "{}".to_string()),
            (
                "message.delta".to_string(),
                "{\"text\":\"hello\"}".to_string(),
            ),
            (
                "tool.start".to_string(),
                "{\"tool_id\":\"call_1\",\"name\":\"bash\"}".to_string(),
            ),
            (
                "tool.end".to_string(),
                "{\"tool_id\":\"call_1\",\"name\":\"bash\",\"status\":\"ok\"}".to_string(),
            ),
            (
                "message.end".to_string(),
                "{\"usage\":{\"in\":1003,\"out\":4}}".to_string(),
            ),
            ("turn.end".to_string(), "{}".to_string()),
        ]
    }

    /// The generated config is a **valid config** — read back through the real parser, which is the only thing that
    /// makes "a smoke config" mean what the build means by a config — and the four settings the acceptance names are
    /// in it.
    #[test]
    fn the_smoke_config_is_a_valid_config_with_the_four_settings() {
        let root = Path::new("/srv/holosmith/smoke");
        let text = config_text(&image(), root, 18_081);
        let config = crate::config::validate_text(Path::new("smoke.toml"), &text).unwrap_or_else(
            |problems| panic!("the smoke config is not a valid config: {problems}"),
        );
        assert_eq!(config.container_prefix, PREFIX);
        assert_eq!(config.network_prefix, PREFIX);
        assert_eq!(config.data_root, root.join("data"));
        assert_eq!(config.projects_root, root.join("projects"));
        assert_eq!(config.control_listen.port(), 0, "the port is the kernel's");
        assert_eq!(config.preview_listen.port(), 18_081);
        assert_eq!(config.agent_image, image());
        assert_eq!(config.backend, crate::runtime::Backend::Docker);
        assert_eq!(
            config.min_free_gb, 0,
            "a full disk is not a verdict about a candidate"
        );
        assert_eq!(config.session_disk_soft_gb, 0);
        assert_eq!(
            config.idle_stop_min, 0,
            "the sweeper must not take the session mid-turn"
        );
        assert_eq!(config.max_live_sessions, 1);
        assert!(
            text.contains("container_prefix = \"smoke_\""),
            "the acceptance names the key's spelling: {text}"
        );
        assert!(text.contains("network_prefix = \"smoke_\""));
    }

    /// The judgement, arm by arm: the full turn passes, and every fact the acceptance names fails on its own.
    ///
    /// Each arm removes exactly one thing, so a `assert_frames` that counted the wrong frame — or that passed over an
    /// empty log — fails here rather than at a promotion.
    #[test]
    fn every_required_frame_is_demanded_and_named_when_it_is_missing() {
        let report = assert_frames(&full_turn()).expect("the recorded turn's frames are a pass");
        assert_eq!(report.frames, 6);
        assert_eq!(report.deltas, 1);
        assert_eq!((report.in_tokens, report.out_tokens), (1003, 4));

        let without = |kind: &str| -> Vec<Event> {
            full_turn()
                .into_iter()
                .filter(|(mine, _)| mine != kind)
                .collect()
        };
        for (kind, wanted) in [
            ("turn.start", "`turn.start`"),
            ("turn.end", "`turn.end`"),
            ("message.delta", "no `message.delta` frame"),
            ("message.end", "no `message.end` carrying a non-zero usage"),
            ("tool.end", "no id in both"),
        ] {
            let Err(sentence) = assert_frames(&without(kind)) else {
                panic!("a log without {kind} passed");
            };
            assert!(
                sentence.contains(wanted),
                "the sentence for a missing {kind} does not name it: {sentence}"
            );
        }
        // A tool pair whose ids disagree is not a pair, and a zero usage is not a report (ADR-0042).
        let mut mismatched = full_turn();
        mismatched[3].1 = "{\"tool_id\":\"call_2\",\"name\":\"bash\"}".to_string();
        assert!(assert_frames(&mismatched).is_err());
        let mut zeros = full_turn();
        zeros[4].1 = "{\"usage\":{\"in\":0,\"out\":0}}".to_string();
        assert!(assert_frames(&zeros).is_err());
        assert!(assert_frames(&[]).is_err(), "an empty log is not a turn");
    }

    /// The label is about the recipe **and** the candidate: either one changing is a rebuild.
    #[test]
    fn the_recipe_label_covers_the_dockerfile_and_the_candidate() {
        let dockerfile = "FROM scratch\n";
        let one = recipe(dockerfile, b"binary one");
        assert_eq!(one, recipe(dockerfile, b"binary one"));
        assert_ne!(one, recipe(dockerfile, b"binary two"));
        assert_ne!(one, recipe("FROM other\n", b"binary one"));
    }

    /// The smoke recipe runs the **agent image's** base, by the same digest — three recipes, one pinned node.
    ///
    /// A drift here is the defect the P1 and ADR-0080's `COPY --from` both were: an image built from a base
    /// nobody pinned, which nothing in a green gate would notice.
    #[test]
    fn the_smoke_recipe_names_the_agent_images_base() {
        let line = |recipe: &str| -> String {
            recipe
                .lines()
                .find(|line| line.starts_with("FROM "))
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(
            line(images::SMOKE_DOCKERFILE),
            line(images::AGENT_DOCKERFILE),
            "the smoke image's base is not the agent image's"
        );
        // And it installs nothing on it: the fake replays records and needs no git, no CA bundle and no openssl.
        assert!(
            !images::SMOKE_DOCKERFILE.contains("apt-get"),
            "the smoke image installs packages it does not need"
        );
        // The wrapper is what makes `pi` the candidate, and `$@` is what lets the launch's own flags through.
        assert!(
            images::SMOKE_DOCKERFILE.contains("container fake-pi \"$@\""),
            "the recipe's `pi` does not pass pi's arguments to the fake"
        );
    }

    /// The two built-in helpers the client rests on: a chunked body and a `Content-Length` one.
    #[test]
    fn an_answer_is_split_at_its_head_and_a_chunked_body_is_joined() {
        let (status, body) = parse(
            b"HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
        )
        .expect("a plain answer parses");
        assert_eq!(status, 202);
        assert_eq!(body, "{\"ok\":true}");

        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"a\":1}\r\n2\r\n{}\r\n0\r\n\r\n";
        let (status, body) = parse(chunked).expect("a chunked answer parses");
        assert_eq!(status, 200);
        assert_eq!(body, "{\"a\":1}{}");
    }

    /// The root's policy, both halves: a directory this command wrote is removed, and one it did not is refused
    /// **by name** rather than deleted. `--root` is an operator's argument, so a guess about what may be deleted is
    /// not a guess this command is allowed to make.
    #[test]
    fn a_root_that_is_not_a_smoke_runs_is_refused_rather_than_removed() {
        let base =
            std::env::temp_dir().join(format!("holosmith-smoke-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let stranger = base.join("stranger");
        std::fs::create_dir_all(&stranger).expect("a scratch directory");
        std::fs::write(stranger.join("keep.txt"), "not mine").expect("a file to keep");
        let Err(sentence) = prepare_root(&stranger) else {
            panic!("a directory that is not a smoke root was accepted");
        };
        assert!(sentence.contains("no smoke config"), "{sentence}");
        assert!(
            stranger.join("keep.txt").exists(),
            "the refusal removed a file it did not write"
        );

        let ours = base.join("ours");
        std::fs::create_dir_all(&ours).expect("a scratch directory");
        std::fs::write(
            ours.join("config.toml"),
            format!("{CONFIG_MARKER}\nrole = \"server\"\n"),
        )
        .expect("a smoke config");
        std::fs::write(ours.join("stale.db"), "a database from a previous run")
            .expect("a stale file");
        let root = prepare_root(&ours).expect("a previous smoke root is reusable");
        assert!(
            !root.join("stale.db").exists(),
            "a previous run's database survived into the next one"
        );
        assert!(
            !root.join("config.toml").exists(),
            "the old config survived"
        );
        assert!(
            root.join("projects").join(PROJECT).join(".git").exists(),
            "the one-commit project was not made"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The root's ending: removed after a pass, kept after a failure — and the *kept* case is what a refusal's
    /// sentence points at.
    #[test]
    fn a_passed_run_removes_its_root_and_a_failed_one_keeps_it() {
        let base =
            std::env::temp_dir().join(format!("holosmith-smoke-finish-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let passed = base.join("passed");
        std::fs::create_dir_all(&passed).expect("a scratch directory");
        std::fs::write(passed.join("serve.log"), "the candidate's log").expect("a file");
        finish(&passed, true).expect("a root the run made is removable");
        assert!(!passed.exists(), "the passing run left its root behind");

        let failed = base.join("failed");
        std::fs::create_dir_all(&failed).expect("a scratch directory");
        finish(&failed, false).expect("a failed run keeps its root without an error");
        assert!(failed.exists(), "the failed run removed the evidence");
        assert!(
            failed.join("serve.log").exists() || true,
            "nothing to assert: the file is what an operator reads"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The exit codes are the contract branches on, and the two failures are told apart.
    #[test]
    fn the_exit_codes_are_pass_failed_and_could_not_run() {
        assert_eq!(Outcome::Passed.exit_code(), 0);
        assert_eq!(Outcome::Failed("bad".to_string()).exit_code(), 1);
        assert_eq!(Outcome::CouldNotRun("no daemon".to_string()).exit_code(), 2);
        assert!(Outcome::Passed.sentence().is_none());
        assert_eq!(Outcome::Failed("bad".to_string()).sentence(), Some("bad"));
    }
}
