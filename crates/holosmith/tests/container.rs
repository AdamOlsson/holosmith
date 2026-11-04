//! V1 — the hardened `.git` mounts, measured against a real daemon (HS-027).
//!
//! `tests/container.rs` is the only place in the tree that runs a real container, and it exists because
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

use holosmith::config::Config;
use holosmith::container::{Mounts, SessionContainer, Source};
use holosmith::sessions::{Session, Status};
use holosmith::worktree::Worktree;

/// The session id the test uses: 16 lowercase hex, the shape the database generates (ADR-0011).
const ID: &str = "0123456789abcdef";

/// A scratch repository under the workspace's `target/`, which the daemon can see.
///
/// Removed by [`Scratch::drop`] unless `HOLOSMITH_KEEP_SCRATCH` is set, which is what to do when a
/// failure needs looking at.
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
            .join(format!("container-v1-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
        Self { path }
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
        let worktree = Worktree::of(&self.path, "holosmith", ID);
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

fn session() -> Session {
    Session {
        id: ID.to_string(),
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

/// The container the mount rule describes, for `projects_root`.
fn container(projects_root: &Path) -> SessionContainer {
    let config = Config {
        projects_root: projects_root.to_path_buf(),
        agent_image: image(),
        ..Config::default()
    };
    SessionContainer::of(&config, &session())
}

/// The script the container runs: the six V1 outcomes, each reported as `name=exit-code`.
///
/// Every command's output is discarded, so the test asserts exit codes and host-side effects rather
/// than parsing an English sentence from a tool version nobody pinned.
fn probe(mounts: &Mounts) -> String {
    let git_dir = mounts.git_dir.host.display();
    format!(
        "set -u\n\
         id -u\n\
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
            Some((name, value)) if value.chars().all(|c| c.is_ascii_digit()) => {
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
    let scratch = Scratch::new("v1");
    let worktree = scratch.project();
    let container = container(&scratch.path);
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

    // `--rm` is HS-028's flag, not this story's, so the test removes its own container both before and
    // after: a name a failed run left behind would otherwise fail the *next* run for the wrong reason.
    rm_container(&container.name);
    let mut arguments = container.run_args();
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
        &["log", "--format=%s", &format!("agent/{ID}")],
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
    // without a `--user` that would have to guess the mapping (the container module's doc).
    let uid = stdout
        .lines()
        .find(|line| !line.contains('=') && line.trim().parse::<u32>().is_ok())
        .unwrap_or_else(|| panic!("the container did not report its uid: {stdout}"));
    assert_ne!(uid.trim(), "0", "the agent runs as root: {stdout}");

    // And the two privileged operations `--cap-drop ALL` is there to refuse.
    for refused in ["remount", "umount"] {
        assert_ne!(exit(refused), "0", "{refused} was allowed: {stdout}");
    }
}
