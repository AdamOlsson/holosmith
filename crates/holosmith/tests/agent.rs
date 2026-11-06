//! The agent process, end to end: pi launched in a real container, the ready handshake, the agent's own
//! directory, project trust, and the exit the server classifies (HS-031).
//!
//! Everything here needs a daemon **and** an image, so every test returns early when there is neither — a
//! machine without Docker is a supported machine (a hub), and the gate's Docker cases guard the same way.
//! `HOLOSMITH_TEST_IMAGE` names another image; `HOLOSMITH_KEEP_SCRATCH=1` keeps the scratch root.
//!
//! **The scratch root is under `target/`**, because the daemon resolves a bind mount's source in the host's
//! mount namespace and this sandbox's `/tmp` is a tmpfs it cannot see (ADR-0035). Since HS-031 that applies
//! to the **data root** as well: `<data>/sessions/<id>/pi-home` is mounted into the container, so a data root
//! the daemon cannot see would give pi an empty directory and no error at all.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use holosmith::agent;
use holosmith::backend::{
    BackendError, DockerBackend, ProcessEnd, Provisioned, SessionBackend, Step,
};
use holosmith::container::{ContainerSettings, Limits, SessionContainer};
use holosmith::sessions::{Session, Status};
use serde_json::json;

/// One id per test: a container is `s_<id>` and a network is `agents_<id>`, so two tests that shared one
/// id would fight over both (HS-027 measured that).
const ID: &str = "0a1b2c3d4e5f6a7b";
const ID_VERSION: &str = "0a1b2c3d4e5f6a7c";
/// A third id, because a container is `s_<id>` and a network is `agents_<id>`: two tests that shared one id
/// would fight over both (HS-027's lesson), and these two run in parallel.
const ID_COMMANDS: &str = "0a1b2c3d4e5f6a7d";

/// A scratch repository, data root and worktree under the workspace's `target/`.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .expect("the workspace root resolves");
        let path = workspace
            .join("target")
            .join(format!("agent-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
        Self { path }
    }

    fn repository(&self) -> PathBuf {
        self.path.join("projects").join("holosmith")
    }

    /// A project repository with one commit and a session worktree on `agent/<ID>`.
    ///
    /// The working tree also gets a project-local prompt (`.pi/prompts/probe.md`), which is the observable
    /// project trust has: pi loads project `.pi` resources only for a trusted project, and `get_commands`
    /// lists them.
    fn project(&self, id: &str) -> PathBuf {
        let repository = self.repository();
        std::fs::create_dir_all(&repository).expect("the repository is creatable");
        for arguments in [
            vec!["init", "--quiet", "-b", "main", "."],
            vec![
                "-c",
                "user.name=verify",
                "-c",
                "user.email=verify@example",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "the first commit",
            ],
        ] {
            let output = Command::new("git")
                .args(&arguments)
                .current_dir(&repository)
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {arguments:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let worktree =
            holosmith::worktree::Worktree::of(&self.path.join("projects"), "holosmith", id);
        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &worktree.branch,
                &worktree.path.to_string_lossy(),
                "HEAD",
            ])
            .current_dir(&repository)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::create_dir_all(worktree.path.join(".pi/prompts"))
            .expect("the worktree is writable");
        std::fs::write(
            worktree.path.join(".pi/prompts/probe.md"),
            "---\ndescription: a project-local prompt\n---\nprobe body\n",
        )
        .expect("the worktree is writable");
        worktree.path
    }

    fn settings(&self, image: &str) -> ContainerSettings {
        ContainerSettings {
            projects_root: self.path.join("projects"),
            data_root: self.path.join("data"),
            image: image.to_string(),
            docker_host: None,
            limits: Limits::default(),
            preview_port: 3000,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("HOLOSMITH_KEEP_SCRATCH").is_none() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn image() -> String {
    std::env::var("HOLOSMITH_TEST_IMAGE").unwrap_or_else(|_| "pi-agent:local".to_string())
}

/// Whether this machine can run a container of the image, and therefore whether the test means anything.
fn ready(image: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", image])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn session(id: &str) -> Session {
    Session {
        id: id.to_string(),
        project: "holosmith".to_string(),
        model: "claude-sonnet-4.5".to_string(),
        thinking: "high".to_string(),
        worktree: String::new(),
        base_commit: None,
        status: Status::Starting,
        status_reason: None,
        client_key: None,
        network: false,
        created_at: 1_760_000_000,
        last_active_at: 1_760_000_000,
        stopped_at: None,
        usage_in: 0,
        usage_out: 0,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("a runtime is buildable")
}

/// Run the network, container and pi steps, and answer the backend that did it.
async fn provision(backend: &DockerBackend, session: &Session) {
    for step in [Step::Network, Step::Container, Step::Pi] {
        assert_eq!(
            backend
                .provision(session, step)
                .await
                .expect("the step works"),
            Provisioned::Nothing,
            "{step}"
        );
    }
}

/// Wait until a container of this name is running, up to thirty seconds.
///
/// The container step *spawns* the attached `docker run` and returns, and the daemon creates the container a
/// moment later — measured here at several seconds, which is why nothing may inspect it straight after the
/// step.
fn wait_running(name: &str) -> bool {
    for _ in 0..600 {
        let output = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Status}}", name])
            .output()
            .expect("docker runs");
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "running" {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn inspect(name: &str, format: &str) -> String {
    let output = Command::new("docker")
        .args(["inspect", "--format", format, name])
        .output()
        .expect("docker runs");
    assert!(
        output.status.success(),
        "docker inspect {format} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn remove(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "--force", name])
        .output();
}

/// The story's demo, as a test: a real container starts pi, pi answers, and the session's agent directory,
/// launch line and container all say so.
#[test]
fn a_real_container_runs_pi_and_answers_the_ready_handshake() {
    let image = image();
    if !ready(&image) {
        println!("SKIP a_real_container_runs_pi_and_answers_the_ready_handshake: no image {image}");
        return;
    }
    let scratch = Scratch::new("ready");
    scratch.project(ID);
    let settings = scratch.settings(&image);
    let backend = DockerBackend::new(settings.clone());
    let session = session(ID);
    let container = SessionContainer::of(&settings, &session);
    remove(&container.name);
    let runtime = runtime();

    runtime.block_on(provision(&backend, &session));
    assert!(wait_running(&container.name), "the container never came up");

    // pi is the container's process, attached, with the agent's own directory in its environment.
    let command = inspect(&container.name, "{{json .Config.Cmd}}");
    for expected in ["--mode", "rpc", "--no-session", "--approve"] {
        assert!(command.contains(expected), "the command is {command}");
    }
    let environment = inspect(&container.name, "{{json .Config.Env}}");
    assert!(
        environment.contains(&format!("PI_CODING_AGENT_DIR={}", container.home.display())),
        "the agent directory is not in the environment: {environment}"
    );
    assert!(environment.contains("PI_OFFLINE=1"), "{environment}");
    assert!(environment.contains("NO_PROXY=key-proxy"), "{environment}");

    // The agent's directory was seeded from the template with the row's two preferences, and the launch line
    // is in the session's log.
    let seeded = std::fs::read_to_string(container.home.join(agent::SETTINGS))
        .expect("the agent directory holds a seed");
    assert!(seeded.contains("defaultModel"), "{seeded}");
    assert!(seeded.contains("claude-sonnet-4.5"), "{seeded}");
    assert!(seeded.contains("high"), "{seeded}");
    let log = std::fs::read_to_string(
        scratch
            .path
            .join("data/sessions")
            .join(ID)
            .join(holosmith::exit::LOG_FILE),
    )
    .expect("the session log exists");
    assert!(log.contains("launched"), "{log}");
    assert!(
        log.contains(&format!("launched {image} (pi ")),
        "the launch line does not name the image and the pi it runs: {log}"
    );
    assert!(log.contains("project trust approved"), "{log}");

    // Project trust, the observable way: the project's own prompt is in the command list, which pi loads only
    // for a trusted project (open question 9's mechanism is `--approve`).
    let running = backend.agent(ID).expect("the session has an agent");
    let reply = runtime
        .block_on(running.request(
            "commands-1",
            json!({"id": "commands-1", "type": "get_commands"}),
            Duration::from_secs(20),
        ))
        .expect("pi answers a command list");
    assert!(reply.success, "{reply:?}");
    let listed = reply.data.expect("the answer carries data").to_string();
    assert!(
        listed.contains("\"probe\""),
        "the project's own prompt is not listed, so it was not trusted: {listed}"
    );

    // The exit the server classifies: killing the container out from under the agent ends the CLI, and the
    // report is this generation's (a code, and whatever it printed). The container is removed before the
    // watcher is taken, which also proves the report is buffered rather than a rendezvous: an exit that
    // arrived before anybody asked is still there.
    remove(&container.name);
    let exit = runtime.block_on(async {
        let watcher = backend
            .supervise(&session)
            .await
            .expect("the launch left a process to supervise");
        tokio::time::timeout(Duration::from_secs(60), watcher.wait())
            .await
            .expect("the exit is reported within a minute")
    });
    assert!(
        matches!(exit, ProcessEnd::Exited { code, .. } if code != 0),
        "{exit:?}"
    );

    // …and a session with no launch has nothing to supervise.
    assert!(
        runtime.block_on(backend.supervise(&session)).is_none(),
        "an exit is reported once"
    );
    runtime
        .block_on(backend.delete(&session))
        .expect("the teardown works");
    assert!(
        !scratch.path.join("data/sessions").join(ID).exists(),
        "the teardown left the session's own directory behind"
    );
}

/// The version assertion's failure path: an image whose pi cannot answer fails the session at the container
/// step, with a sentence that names the image. (A *mismatched* version is the same path with a different
/// sentence — `agent::version_sentence`'s unit test pins the wording, because producing a second image with a
/// different pi is not something a test on this machine can do.)
#[test]
fn an_image_whose_pi_cannot_answer_fails_the_session_at_the_container_step() {
    let image = image();
    if !ready(&image) {
        println!("SKIP an_image_whose_pi_cannot_answer...: no image {image}");
        return;
    }
    let scratch = Scratch::new("version");
    scratch.project(ID_VERSION);
    let settings = scratch.settings("holosmith-no-such-image:local");
    let backend = DockerBackend::new(settings.clone());
    let session = session(ID_VERSION);
    let runtime = runtime();

    runtime.block_on(async {
        backend
            .provision(&session, Step::Network)
            .await
            .expect("the network step does not need the image");
        let error = backend
            .provision(&session, Step::Container)
            .await
            .expect_err("an image that cannot answer is a failed session");
        let BackendError::Failed { reason } = error else {
            panic!("the failure is not a reason: {error:?}");
        };
        assert!(
            reason.contains("holosmith-no-such-image:local"),
            "the sentence does not name the image: {reason}"
        );
        assert!(
            reason.contains("which pi it runs"),
            "the sentence does not say what could not be answered: {reason}"
        );
        backend.delete(&session).await.expect("the teardown works");
    });
}

/// The turn's four commands, against real pi: each one reaches the agent and gets an answer back (HS-032).
///
/// This container has no provider key (HS-039 builds the key proxy) and its network is internal (HS-028), so
/// `prompt` is *refused* by pi before a stream exists — which is the arm asserted here. What the test is
/// about is reachability rather than the answer: a command that never got written would fail with a write
/// error or a timeout, and a refusal is pi's own sentence arriving on the pipe. The streaming arm is
/// `tests/rpc.rs`'s, over records captured from a real turn.
#[test]
fn the_turns_commands_reach_pi_and_a_refusal_is_pis_sentence() {
    let image = image();
    if !ready(&image) {
        println!(
            "SKIP the_turns_commands_reach_pi_and_a_refusal_is_pis_sentence: no image {image}"
        );
        return;
    }
    let scratch = Scratch::new("commands");
    scratch.project(ID_COMMANDS);
    let settings = scratch.settings(&image);
    let backend = DockerBackend::new(settings.clone());
    let session = session(ID_COMMANDS);
    let container = SessionContainer::of(&settings, &session);
    remove(&container.name);
    let runtime = runtime();

    runtime.block_on(async {
        provision(&backend, &session).await;
        // One event writer for the prompt, on the scratch root's own database: nothing is asserted about the
        // log here, only about the answer, and a turn that is refused appends nothing anyway.
        let database = std::sync::Arc::new(std::sync::Mutex::new(
            holosmith::db::Database::open(
                &scratch.path.join("commands.db"),
                holosmith::db::OpenOptions::default(),
                1_760_000_000,
            )
            .expect("a database opens"),
        ));
        let (events, _stream) =
            holosmith::events::spawn(database, holosmith::events::WriterOptions::default());

        let prompt = backend
            .start_turn(&session, "say hello", &events)
            .await
            .expect_err("a session with no provider key cannot start a turn");
        let BackendError::Failed { reason } = prompt else {
            panic!("the prompt failed with something other than a reason: {prompt:?}");
        };
        assert!(
            reason.contains("the prompt was refused"),
            "the reason does not say the prompt was refused: {reason}"
        );
        assert!(
            reason.contains("API key") || reason.contains("provider"),
            "the reason does not carry pi's own words: {reason}"
        );

        // The three ladder commands: an idle agent answers them, and an answer either way is the point — a
        // command that never reached the pipe would be a write error or a timeout, and both are failures here.
        answer(backend.clear_queue(&session).await, "clear_queue");
        answer(backend.abort(&session).await, "abort");
        answer(backend.steer(&session, "go left").await, "steer");

        // The agent is still the same process and still answering: a refused prompt is not a dead agent.
        let running = backend
            .agent(ID_COMMANDS)
            .expect("the session still has an agent");
        let reply = running
            .request(
                "state-after-1",
                json!({"id": "state-after-1", "type": "get_state"}),
                Duration::from_secs(20),
            )
            .await
            .expect("pi answers `get_state` after a refused prompt");
        assert!(reply.success, "{reply:?}");

        backend.delete(&session).await.expect("the teardown works");
    });
}

/// Accept pi's two answers to a command — accepted, or refused in pi's words — and fail on anything that says
/// the command never reached it.
fn answer(result: Result<(), BackendError>, operation: &str) {
    match result {
        Ok(()) => {}
        Err(BackendError::Failed { reason }) => assert!(
            reason.contains("was refused"),
            "the {operation} failed with something other than pi's refusal: {reason}"
        ),
        Err(other) => panic!("the {operation} did not reach pi: {other}"),
    }
}
