//! V1 — the hardened `.git` mounts, measured against a real daemon (HS-027).
//!
//! `tests/container.rs` is the only *test* that runs a real container (the gate's two cases do too), and it exists because
//! spec item 1's mounts are a claim about the kernel rather than about our code: three writes must fail,
//! three git commands must work, and a scripted check is the only thing that can say so. The story calls
//! this the **V1 gate**.
//!
//! Two environment facts shape it, and both were measured (ADR-0035):
//!
//! - **The daemon resolves host paths in the host's mount namespace, and a bind-mounted path is only
//!   visible to it when it is a real host directory.** A `TempDir` under `/tmp` is not: in a sandbox whose
//!   `/tmp` is a tmpfs, `docker run -v /tmp/...` silently creates an empty root-owned directory instead
//!   of mounting anything, and every assertion then fails for a reason that has nothing to do with V1.
//!   The scratch repository therefore lives under the workspace's `target/`, which is the same directory
//!   the compiler writes to and is a real host path wherever `cargo test` runs.
//! - **The properties are asserted where they are observable, not in English.** A refused write is the
//!   file's absence *on the host*, a refused rename is the mount point still being there, and a working
//!   commit is the branch's head moving in the host's repository. Docker's and git's sentences are
//!   reported in the failure message, never parsed.
//!
//! The test **skips** — one printed line, no failure — when there is no daemon or no image, because a
//! machine without Docker is a supported machine (a hub, spec item 10 §6) and `cargo test` must stay green
//! there. `scripts/verify.sh`'s `case_container_mounts` is the same check for the gate, and it skips the
//! same way; neither may be the *only* proof, which is why both exist.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use holosmith::backend::{DockerBackend, Provisioned, SessionBackend, Step};
use holosmith::container::{
    ContainerSettings, Limits, Mounts, SessionContainer, Source, network_name,
};
use holosmith::docker::Docker;
use holosmith::sessions::{Session, Status};
use holosmith::worktree::Worktree;

/// The session ids the tests use: 16 lowercase hex, the shape the database generates (ADR-0011).
///
/// Two of them, because two tests run at once in one process and a container name or a network name is
/// `s_<id>`/`agents_<id>`: one id for V1's probe and one for the container run.
const ID_V1: &str = "0123456789abcdef";
const ID_RUN: &str = "fedcba9876543210";
const ID_RESUME: &str = "aaaabbbbccccdddd";
const ID_START: &str = "1111222233334444";
const ID_RACE: &str = "9999888877776666";

/// A scratch repository under the workspace's `target/`, which the daemon can see.
///
/// Removed by [`Scratch::drop`] unless `HOLOSMITH_KEEP_SCRATCH` is set, which is what to do when a
/// failure needs looking at.
struct Scratch {
    path: PathBuf,
    id: &'static str,
}

impl Scratch {
    fn new(label: &str, id: &'static str) -> Self {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .expect("the workspace root resolves");
        let path = workspace
            .join("target")
            .join(format!("container-v1-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
        Self { path, id }
    }

    fn repository(&self) -> PathBuf {
        self.path.join("holosmith")
    }

    /// A project repository with one commit in it, and a session worktree on `agent/<ID>`.
    fn project(&self) -> PathBuf {
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
            let output = git(&repository, &arguments);
            assert!(
                output.status.success(),
                "git {arguments:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let worktree = Worktree::of(&self.path, "holosmith", self.id);
        let output = git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                &worktree.branch,
                &worktree.path.to_string_lossy(),
                "HEAD",
            ],
        );
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        worktree.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("HOLOSMITH_KEEP_SCRATCH").is_none() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Remove a container by name, ignoring "no such container" (HS-029 does this before every start; until
/// then a container a failed run left behind keeps the name, which is measured: HS-027's first V1 run
/// failed its second attempt with "the container name is already in use").
fn rm_container(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "--force", name])
        .output();
}

fn git(repository: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("git runs")
}

/// The image the V1 container runs. `<image>` is what a deployed machine has (item 10 §3); the default
/// names the image this repository's own development machine carries.
fn image() -> String {
    std::env::var("HOLOSMITH_TEST_IMAGE").unwrap_or_else(|_| "pi-agent:local".to_string())
}

/// The daemon answers, or this test says so once and returns.
fn ready() -> bool {
    let daemon = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !daemon {
        eprintln!(
            "SKIP tests/container.rs: no Docker daemon answers (V1 is case_container_mounts)"
        );
        return false;
    }
    let image = image();
    let present = Command::new("docker")
        .args(["image", "inspect", &image])
        .output()
        .is_ok_and(|output| output.status.success());
    if !present {
        eprintln!("SKIP tests/container.rs: no image {image} (V1 is case_container_mounts)");
    }
    present
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

/// The machine's container settings for `projects_root`, with the defaults' limits.
fn settings(projects_root: &Path) -> ContainerSettings {
    ContainerSettings {
        projects_root: projects_root.to_path_buf(),
        image: image(),
        docker_host: None,
        limits: Limits::default(),
    }
}

/// The container the mount rule describes, for `projects_root`.
fn container(projects_root: &Path, id: &str) -> SessionContainer {
    SessionContainer::of(&settings(projects_root), &session(id))
}

/// The script the container runs: the six V1 outcomes, each reported as `name=exit-code`.
///
/// Every command's output is discarded, so the test asserts exit codes and host-side effects rather
/// than parsing an English sentence from a tool version nobody pinned.
fn probe(mounts: &Mounts) -> String {
    let git_dir = mounts.git_dir.host.display();
    format!(
        "set -u\n\
         id -u 2>/dev/null || echo missing\n\
         ( echo x > {git_dir}/hooks/pre-commit ) >/dev/null 2>&1; echo hooks_write=$?\n\
         ( echo '# x' >> {git_dir}/config ) >/dev/null 2>&1; echo config_append=$?\n\
         ( mv {git_dir}/hooks {git_dir}/hooks2 ) >/dev/null 2>&1; echo hooks_rename=$?\n\
         git -c user.email=verify@example -c user.name=verify commit --quiet --allow-empty \
           -m 'the second commit' >/dev/null 2>&1; echo commit=$?\n\
         ( echo x > second.txt && git add second.txt && \
           git -c user.email=verify@example -c user.name=verify commit --quiet \
             -m 'a file commit' ) >/dev/null 2>&1; echo commit_file=$?\n\
         git branch agent/v1-probe >/dev/null 2>&1; echo branch=$?\n\
         git worktree list; echo worktree_list=$?\n\
         command -v mount >/dev/null 2>&1 && command -v umount >/dev/null 2>&1 \
           && echo tools=present || echo tools=missing\n\
         mount -o remount,rw / >/dev/null 2>&1; echo remount=$?\n\
         umount /work >/dev/null 2>&1; echo umount=$?\n"
    )
}

/// `name=value` lines from the container's stdout, plus everything that was not one.
fn reported(stdout: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut pairs = Vec::new();
    let mut rest = Vec::new();
    for line in stdout.lines() {
        match line.split_once('=') {
            // A name is a word; the value is whatever the probe printed (an exit code, or `present`). `git
            // worktree list`'s lines have no `=` at all, which is how they end up in `rest`.
            Some((name, value))
                if !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !value.is_empty() =>
            {
                pairs.push((name.to_string(), value.to_string()))
            }
            _ => rest.push(line.to_string()),
        }
    }
    (pairs, rest)
}

#[test]
fn v1_holds_the_hardened_git_mounts_answer_the_way_spec_item_one_says() {
    if !ready() {
        return;
    }
    let scratch = Scratch::new("v1", ID_V1);
    let worktree = scratch.project();
    let container = container(&scratch.path, ID_V1);
    let mounts = container.mounts.clone();

    // The mount check the container step asks before it creates anything: all four sources are there, of
    // the kind their mount needs, because `docker run -v` would create a missing one as an empty
    // directory and mount nothing.
    mounts
        .check()
        .expect("the four mount sources exist on a real worktree");
    assert_eq!(mounts.config.source, Source::File);
    let config_before =
        std::fs::read_to_string(&mounts.config.host).expect("the config is readable");

    // The run vector now names the session's own network (HS-028), so V1's container joins a real one:
    // `--internal`, created by the same helper the backend's network step uses.
    let docker = Docker::new(None);
    docker
        .create_internal_network(&container.network)
        .expect("the session network is creatable");
    rm_container(&container.name);
    // The production vector is detached (the container step does not hold a process open until HS-031);
    // V1 needs the container's stdout, so the same vector runs attached.
    let mut arguments: Vec<String> = container
        .run_args()
        .into_iter()
        // V1 needs the container's own stdout, so the same vector runs attached.
        .filter(|argument| argument != "--detach")
        .collect();
    // …and the placeholder command is the agent step's, not V1's: the vector is cut after the image, which
    // is the boundary the container module documents, and the probe replaces what runs.
    let image = arguments
        .iter()
        .position(|argument| argument == &container.image)
        .expect("the run vector names its image");
    arguments.truncate(image + 1);
    arguments.push("sh".to_string());
    arguments.push("-c".to_string());
    arguments.push(probe(&mounts));
    let output = Command::new("docker")
        .args(&arguments)
        .output()
        .expect("docker runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    rm_container(&container.name);
    assert!(
        output.status.success(),
        "the container did not run: {stderr}\nstdout: {stdout}"
    );
    let (pairs, rest) = reported(&stdout);
    let exit = |name: &str| -> String {
        pairs
            .iter()
            .find(|(reported, _)| reported == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("the container did not report {name}: {stdout}"))
    };

    // The three writes spec item 1 says must fail. Each is a non-zero exit, and the host is where the
    // consequence is observable.
    for refused in ["hooks_write", "config_append", "hooks_rename"] {
        assert_ne!(exit(refused), "0", "{refused} was allowed: {stdout}");
    }
    assert!(
        !mounts.hooks.host.join("pre-commit").exists(),
        "a hook reached the host: {}",
        mounts.hooks.host.display()
    );
    assert_eq!(
        std::fs::read_to_string(&mounts.config.host).expect("the config is readable"),
        config_before,
        "the repository's config was rewritten through a read-only mount"
    );
    assert!(
        mounts.hooks.host.is_dir(),
        "the hooks directory was moved out from under the host"
    );

    // The three commands spec item 1 says must work: a commit (an empty one and one that writes a file,
    // because `add` plus `commit` touches `index` and `objects`, which only the writable `.git` gives).
    for worked in ["commit", "commit_file", "branch", "worktree_list"] {
        assert_eq!(
            exit(worked),
            "0",
            "{worked} failed inside the container: {stdout}"
        );
    }
    assert!(
        worktree.join("second.txt").is_file(),
        "the file the container committed is not on the host's worktree"
    );
    let log = git(
        &scratch.repository(),
        &["log", "--format=%s", &format!("agent/{ID_V1}")],
    );
    let log = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(
        log.contains("a file commit") && log.contains("the second commit"),
        "the host's branch does not have the container's commits: {log}"
    );
    let branches = git(
        &scratch.repository(),
        &["branch", "--list", "agent/v1-probe"],
    );
    assert!(
        String::from_utf8_lossy(&branches.stdout).contains("agent/v1-probe"),
        "the branch the container created is not in the host's repository"
    );
    let listed = rest.join("\n");
    assert!(
        listed.contains(&worktree.display().to_string()),
        "`git worktree list` did not name the worktree at {}: {listed}",
        worktree.display()
    );

    // The process is not root: the image's own user runs the agent, and HS-028's flag list says so
    // without a `--user` that would have to guess the mapping (the container module's doc). A missing `id`
    // prints `missing` and fails here, rather than being read as a uid that happens not to be zero.
    let uid = stdout
        .lines()
        .find(|line| !line.contains('=') && !line.trim().is_empty())
        .unwrap_or_else(|| panic!("the container did not report its uid: {stdout}"));
    assert!(
        uid.trim().parse::<u32>().is_ok(),
        "the container's uid is not a number, so \"not root\" proves nothing: {uid}"
    );
    assert_ne!(uid.trim(), "0", "the agent runs as root: {stdout}");

    // And the two privileged operations `--cap-drop ALL` is there to refuse. 127 is "no such file", which
    // is a missing tool rather than a refused mount, so it fails instead of passing for the wrong reason.
    assert_eq!(
        exit("tools"),
        "present",
        "the image has no mount/umount, so this clause cannot be measured: {stdout}"
    );
    for refused in ["remount", "umount"] {
        let code = exit(refused);
        assert_ne!(code, "0", "{refused} was allowed: {stdout}");
        assert_ne!(
            code, "127",
            "{refused} is not a program in this image: {stdout}"
        );
    }
    assert!(
        docker.remove_network(&container.network),
        "the network went away"
    );
}

/// The limits the run test asks for: deliberately *not* the defaults, so "the limits come from config" is
/// what the assertions prove.
const TEST_LIMITS: Limits = Limits {
    cpus: 1,
    memory_mb: 512,
    pids: 64,
};

#[test]
fn the_container_step_runs_a_limited_container_with_no_way_out() {
    if !ready() {
        return;
    }
    let scratch = Scratch::new("run", ID_RUN);
    scratch.project();
    let settings = ContainerSettings {
        projects_root: scratch.path.clone(),
        image: image(),
        docker_host: None,
        limits: TEST_LIMITS,
    };
    let backend = DockerBackend::new(settings.clone());
    let docker = settings.docker();
    let session = session(ID_RUN);
    let container = SessionContainer::of(&settings, &session);
    rm_container(&container.name);
    docker.remove_network(&container.network);

    // The two steps, through the real backend: the network first (a container cannot join one that is not
    // there), then the container. Both are the shipped implementation, not a copy of its arguments.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime is buildable");
    runtime.block_on(async {
        assert_eq!(
            backend
                .provision(&session, Step::Network)
                .await
                .expect("the network step works"),
            Provisioned::Nothing
        );
        assert_eq!(
            backend
                .provision(&session, Step::Container)
                .await
                .expect("the container step works"),
            Provisioned::Nothing
        );
    });
    assert_eq!(container.network, network_name(ID_RUN));

    // Everything spec item 6 and HS-028's acceptance name, read out of `docker inspect` — the only place a
    // running container's real configuration is visible.
    let field = |format: &str| {
        docker
            .inspect(&container.name, format)
            .unwrap_or_else(|error| panic!("docker inspect {format}: {error}"))
    };
    assert_eq!(
        field("{{.State.Status}}"),
        "running",
        "the container is not running"
    );
    assert_eq!(field("{{.Config.WorkingDir}}"), "/work");
    assert_eq!(field("{{.Config.Image}}"), image());
    assert!(
        field("{{.Config.Env}}").contains(&format!("HS_SESSION_ID={ID_RUN}")),
        "the session id is not in the container's environment"
    );
    // Spec item 6's run line: `--rm --init`, and the network it joins is its own.
    assert_eq!(field("{{.HostConfig.AutoRemove}}"), "true", "`--rm`");
    assert_eq!(field("{{.HostConfig.Init}}"), "true", "`--init`");
    assert_eq!(field("{{.HostConfig.NetworkMode}}"), container.network);
    // The hardening spec item 1 and V1 depend on.
    assert_eq!(field("{{.HostConfig.Privileged}}"), "false");
    assert_eq!(
        field("{{len .HostConfig.CapAdd}}"),
        "0",
        "a capability was added"
    );
    assert!(
        field("{{.HostConfig.CapDrop}}").contains("ALL"),
        "capabilities are not dropped: {}",
        field("{{.HostConfig.CapDrop}}")
    );
    assert!(
        field("{{.HostConfig.SecurityOpt}}").contains("no-new-privileges"),
        "no-new-privileges is missing: {}",
        field("{{.HostConfig.SecurityOpt}}")
    );
    // The limits, from the config rather than from the image or the defaults.
    assert_eq!(
        field("{{.HostConfig.NanoCpus}}"),
        (u64::from(TEST_LIMITS.cpus) * 1_000_000_000).to_string()
    );
    assert_eq!(
        field("{{.HostConfig.Memory}}"),
        (TEST_LIMITS.memory_mb * 1024 * 1024).to_string()
    );
    assert_eq!(
        field("{{.HostConfig.PidsLimit}}"),
        TEST_LIMITS.pids.to_string()
    );
    // No way out: no socket, no host network, and the four mounts and nothing else.
    let binds = field("{{.HostConfig.Binds}}");
    assert!(
        !binds.contains("docker.sock"),
        "the Docker socket is mounted into the session: {binds}"
    );
    assert_eq!(
        field("{{len .HostConfig.Binds}}"),
        "4",
        "the mounts are not the four: {binds}"
    );
    assert_ne!(field("{{.HostConfig.NetworkMode}}"), "host");
    assert_eq!(
        field("{{.HostConfig.PidMode}}"),
        "",
        "the container shares the host's pid namespace"
    );

    // A demo of this story is this line plus the assertions above: the facts a `docker inspect` shows.
    println!(
        "container {}: image={} network={} cpus={} memory={}m pids={} caps=drop-all add=0 \
         privileged=false no-new-privileges=true workdir=/work auto-remove=true init=true binds={}",
        container.name,
        image(),
        container.network,
        TEST_LIMITS.cpus,
        TEST_LIMITS.memory_mb,
        TEST_LIMITS.pids,
        field("{{len .HostConfig.Binds}}"),
    );

    // The teardown the delete flow and a failed provision both use, and the promise both make: the
    // container and the network go away, and asking twice is not a failure.
    runtime.block_on(async {
        backend
            .delete(&session)
            .await
            .expect("the delete removes what the session owns");
    });
    assert!(
        !docker.container_exists(&container.name),
        "the container outlived the delete"
    );
    assert!(
        !docker.succeeds(&["network", "inspect", &container.network]),
        "the network outlived the delete"
    );
}

/// A runtime for the tests that drive the backend's `async` methods.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("a runtime is buildable")
}

/// A started session: worktree, network and container, through the real backend.
fn start(backend: &DockerBackend, session: &Session) {
    runtime().block_on(async {
        backend
            .provision(session, Step::Network)
            .await
            .expect("the network step works");
        backend
            .provision(session, Step::Container)
            .await
            .expect("the container step works");
    });
}

/// Wait for a container to be gone, up to two seconds.
///
/// `--rm` removes a container when it exits, and the daemon does that *after* `docker stop` has answered —
/// measured, and the reason spec item 6 asks for `rm -f` before every start rather than trusting the flag.
/// A test that reads `container_exists` the instant a stop returns races that removal (this test did, one
/// run in three).
fn wait_gone(docker: &Docker, name: &str) -> bool {
    for _ in 0..100 {
        if !docker.container_exists(name) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// `docker exec s_<id> sh -c <script>`, answering its stdout.
fn exec(container: &str, script: &str) -> String {
    let output = Command::new("docker")
        .args(["exec", container, "sh", "-c", script])
        .output()
        .expect("docker exec runs");
    assert!(
        output.status.success(),
        "docker exec failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn a_second_start_never_fails_on_a_leftover_name() {
    // Spec item 6's first line, and the reason it exists: `--rm` covers a container that *stopped*, and
    // nothing covers one this process lost track of — a `kill -9`, a crash, a row removed while its
    // container lived (HS-044 reconciles those at startup). So a start removes any container of its name
    // first, and asking twice is boring.
    if !ready() {
        return;
    }
    let scratch = Scratch::new("start", ID_START);
    scratch.project();
    let settings = settings(&scratch.path);
    let backend = DockerBackend::new(settings.clone());
    let docker = settings.docker();
    let session = session(ID_START);
    let container = SessionContainer::of(&settings, &session);
    rm_container(&container.name);
    docker.remove_network(&container.network);

    start(&backend, &session);
    let first = docker
        .inspect(&container.name, "{{.Id}}")
        .expect("the first container");
    assert_eq!(
        docker
            .inspect(&container.name, "{{.State.Status}}")
            .expect("the container answers for its status"),
        "running"
    );

    // Stop it: `--rm` makes the daemon remove it — but not before `docker stop` returns. Measured here: the
    // removal is the daemon's own work after the process has exited, so it lands a moment later. That window
    // is exactly why the spec asks for `rm -f` before every start rather than trusting the flag.
    runtime().block_on(async {
        backend.stop(&session).await.expect("a stop works");
    });
    assert!(
        wait_gone(&docker, &container.name),
        "`--rm` left the container behind for two seconds"
    );

    // …and a start after that — the "leftover" case the spec's `rm -f` is for — succeeds.
    start(&backend, &session);
    let second = docker
        .inspect(&container.name, "{{.Id}}")
        .expect("the second container");
    assert_ne!(first, second, "the second start reused the first container");
    assert_eq!(
        docker
            .inspect(&container.name, "{{.State.Status}}")
            .expect("the container answers for its status"),
        "running"
    );

    // The leftover this story is really about: a container this process cannot stop, because it does not
    // know it is there. Plant one under the name and start anyway.
    runtime().block_on(async {
        backend.stop(&session).await.expect("a stop works");
    });
    let output = Command::new("docker")
        .args(container.run_args())
        .output()
        .expect("docker runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        docker
            .inspect(&container.name, "{{.Id}}")
            .expect("the container has an id"),
        String::from_utf8_lossy(&output.stdout).trim(),
        "the planted container is not the one under the name"
    );
    start(&backend, &session);
    assert_ne!(
        docker
            .inspect(&container.name, "{{.Id}}")
            .expect("the container has an id"),
        String::from_utf8_lossy(&output.stdout).trim(),
        "the start did not replace the leftover container"
    );

    runtime().block_on(async {
        backend.delete(&session).await.expect("the teardown works");
    });
}

#[test]
fn removing_the_container_loses_nothing_the_worktree_holds() {
    // The acceptance's third line, and the one claim this system rests on: session data lives in host
    // directories, so a container is disposable. The probe is a commit the *container* makes — if the host's
    // branch still has it after a stop, a removal and a resume, the claim is a fact.
    if !ready() {
        return;
    }
    let scratch = Scratch::new("resume", ID_RESUME);
    scratch.project();
    let settings = settings(&scratch.path);
    let backend = DockerBackend::new(settings.clone());
    let docker = settings.docker();
    let session = session(ID_RESUME);
    let container = SessionContainer::of(&settings, &session);
    rm_container(&container.name);
    docker.remove_network(&container.network);

    start(&backend, &session);
    exec(
        &container.name,
        "cd /work && echo written-by-the-container > from-container.txt && \
         git add from-container.txt && \
         git -c user.email=verify@example -c user.name=verify commit --quiet -m from-container",
    );
    let head = exec(&container.name, &format!("git rev-parse agent/{ID_RESUME}"));

    // The container goes away entirely — stopped, then removed — and comes back.
    runtime().block_on(async {
        backend.stop(&session).await.expect("a stop works");
    });
    assert!(
        wait_gone(&docker, &container.name),
        "the stop left the container behind for two seconds"
    );
    assert!(
        docker.remove_container(&container.name) || !docker.container_exists(&container.name),
        "the removal is idempotent"
    );

    runtime().block_on(async {
        backend.resume(&session).await.expect("a resume works");
    });
    assert_eq!(
        docker
            .inspect(&container.name, "{{.State.Status}}")
            .expect("the container answers for its status"),
        "running",
        "the resume did not start a container"
    );
    // The container the resume made finds the work: the commit, the branch, and the file.
    assert_eq!(
        exec(&container.name, &format!("git rev-parse agent/{ID_RESUME}")),
        head,
        "the branch moved when the container was replaced"
    );
    assert_eq!(
        exec(&container.name, "cat /work/from-container.txt"),
        "written-by-the-container"
    );
    assert!(
        scratch
            .path
            .join(".worktrees")
            .join(ID_RESUME)
            .join("from-container.txt")
            .is_file(),
        "the file the container wrote is not on the host"
    );

    runtime().block_on(async {
        backend.delete(&session).await.expect("the teardown works");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_starts_of_one_session_do_not_race() {
    // Spec item 6's fourth line. Without the lock these eight starts interleave `rm -f` and `run` and some
    // fail with "the container name is already in use" — the failure is timing-dependent by nature, so the
    // assertion is the invariant that matters: every start succeeded and exactly one container is running.
    if !ready() {
        return;
    }
    let scratch = Scratch::new("race", ID_RACE);
    scratch.project();
    let settings = settings(&scratch.path);
    let backend = Arc::new(DockerBackend::new(settings.clone()));
    let docker = settings.docker();
    let container = SessionContainer::of(&settings, &session(ID_RACE));
    rm_container(&container.name);
    docker.remove_network(&container.network);
    backend
        .provision(&session(ID_RACE), Step::Network)
        .await
        .expect("the network step works");

    let mut handles = Vec::new();
    for _ in 0..8 {
        let backend = Arc::clone(&backend);
        handles.push(tokio::spawn(async move {
            backend.provision(&session(ID_RACE), Step::Container).await
        }));
    }
    let mut failures = Vec::new();
    for handle in handles {
        match handle.await.expect("the task does not panic") {
            Ok(_) => {}
            Err(error) => failures.push(error.to_string()),
        }
    }
    assert!(failures.is_empty(), "a start raced another: {failures:?}");
    assert_eq!(
        docker
            .inspect(&container.name, "{{.State.Status}}")
            .expect("the container answers for its status"),
        "running"
    );
    let running = Command::new("docker")
        .args([
            "ps",
            "--quiet",
            "--filter",
            &format!("name=^{}$", container.name),
        ])
        .output()
        .expect("docker ps runs");
    assert_eq!(
        String::from_utf8_lossy(&running.stdout).lines().count(),
        1,
        "more than one container answers to the session's name"
    );

    backend
        .delete(&session(ID_RACE))
        .await
        .expect("the teardown works");
}
