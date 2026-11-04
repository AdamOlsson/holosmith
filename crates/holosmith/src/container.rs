//! The session's container: the four mounts and the hardening flags, in one value (HS-027).
//!
//! Spec item 1 names the mounts and says why they are shaped the way they are, and V1 is the check that
//! they actually behave that way. This module is that list as data — a [`Mounts`] value built from the
//! machine's projects root and the session's id — plus the [`SessionContainer`] that turns it into a
//! `docker run` argument vector. Nothing here talks to the daemon: HS-028's container step runs it,
//! HS-036 adds the session's network, and [`crate::preflight`] already said whether the daemon answers.
//!
//! ```text
//! -v <worktree>:/work
//! -v <project>/.git:<project>/.git
//! -v <project>/.git/hooks:<project>/.git/hooks:ro
//! -v <project>/.git/config:<project>/.git/config:ro
//! ```
//!
//! **The host path and the container path are the same string, and that identity is load-bearing.** A
//! worktree's `.git` is a *file* holding `gitdir: <absolute path>`, and that path is
//! `<project>/.git/worktrees/<id>` on the host. Mount the repository anywhere else and git inside the
//! container looks at a directory that is not mounted — the session appears to have no repository, or
//! worse, git resolves a `gitdir` that another mount happens to answer. So `/work` is the one exception:
//! it is a fresh path in the container, and the worktree's `gitdir` still points at the host path, which
//! is mounted at that same path.
//!
//! **Why the two read-only mounts are two mounts.** `.git/hooks` and `.git/config` are what let a
//! repository run a program on the host: hooks by definition, and config through `core.fsmonitor`,
//! `core.sshCommand`, aliases and credential helpers. Mounting the smaller paths read-only *stacked on
//! top of* the writable `.git` is what makes them unwritable while `objects`, `refs`, `logs` and
//! `worktrees/<id>` stay writable — which is what `git commit` needs. V1 measured that this works
//! (ADR-0035): the writes fail with `Read-only file system`, renaming the mount point fails with
//! `Device or resource busy`, and `commit`/`branch`/`worktree list` all answer.
//!
//! **A mount whose host path is missing is not a mount.** `docker run -v` *creates* a missing source as
//! an empty root-owned directory, so a typo in the projects root would give the agent an empty `/work` and
//! a `.git` that is not the repository — silently. [`Mounts::check`] is the question that rules that out,
//! and it is asked before the container is created.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::docker::Docker;
use crate::sessions::Session;
use crate::worktree::Worktree;

/// Where the session's worktree is mounted: the directory the agent edits (spec item 1).
pub const WORKDIR: &str = "/work";

/// The variable that tells anything inside the container which session it is (spec item 1).
pub const SESSION_ID_ENV: &str = "HS_SESSION_ID";

/// The prefix of every container name: `s_<id>` (spec item 6).
pub const NAME_PREFIX: &str = "s_";

/// The prefix of every session network: `agents_<id>` (spec item 2).
pub const NETWORK_PREFIX: &str = "agents_";

/// The command the container runs until HS-031 attaches the agent's stdio.
///
/// A placeholder, and it says so: from this story on the container exists, is mounted, is hardened and is
/// limited, and nothing inside it is working yet — which is exactly what this build can truthfully claim.
/// The image must have `sleep` (any POSIX system does). HS-031 replaces this with pi in RPC mode over the
/// container's stdin, and the `--detach` above with an attached process.
pub const IDLE_COMMAND: [&str; 2] = ["sleep", "infinity"];

/// The name of a session's private network.
pub fn network_name(session_id: &str) -> String {
    format!("{NETWORK_PREFIX}{session_id}")
}

/// What a mount's source must be on disk.
///
/// It matters because the two are not interchangeable in Docker's eyes: `-v` of a directory and `-v` of a
/// file are different mount kinds, and a missing source is created as a directory either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Directory,
    File,
}

impl Source {
    fn describe(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
        }
    }

    /// Whether `path` is this kind of thing *on the host*.
    fn holds(self, path: &Path) -> bool {
        match self {
            Self::Directory => path.is_dir(),
            // `is_file` follows a symlink: a linked `.git/config` is still a file git reads.
            Self::File => path.is_file(),
        }
    }
}

/// One bind mount: where the host has it, where the container finds it, and whether it is writable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub host: PathBuf,
    pub at: PathBuf,
    pub read_only: bool,
    pub source: Source,
}

impl Mount {
    /// The `-v host:at[:ro]` argument Docker takes.
    ///
    /// One string rather than `--mount type=bind,src=…`, because the spec writes `-v` and because the
    /// short form is what the measurement in ADR-0035 was taken with.
    pub fn argument(&self) -> String {
        let host = self.host.display();
        let at = self.at.display();
        if self.read_only {
            format!("{host}:{at}:ro")
        } else {
            format!("{host}:{at}")
        }
    }
}

/// The four mounts of spec item 1, in the order the spec lists them.
///
/// A struct rather than a `Vec<Mount>` so a caller cannot drop one: all four are the isolation, and three
/// of them without the fourth is a weaker thing than the spec describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mounts {
    /// `<worktree>:/work`: the agent's files, writable, at a fresh path.
    pub worktree: Mount,
    /// `<project>/.git:<project>/.git`: objects, refs, logs and `worktrees/<id>`, writable.
    pub git_dir: Mount,
    /// `<project>/.git/hooks:<project>/.git/hooks:ro`: what could run a program on the host.
    pub hooks: Mount,
    /// `<project>/.git/config:<project>/.git/config:ro`: the same, by another route.
    pub config: Mount,
}

impl Mounts {
    /// The mounts for the session `worktree` names.
    ///
    /// The paths are the worktree's own ([`Worktree`]), so the naming rule lives in one place: this
    /// function derives nothing and decides nothing.
    pub fn of(worktree: &Worktree) -> Self {
        let git_dir = worktree.repository.join(".git");
        Self {
            worktree: Mount {
                host: worktree.path.clone(),
                at: PathBuf::from(WORKDIR),
                read_only: false,
                source: Source::Directory,
            },
            git_dir: Mount {
                host: git_dir.clone(),
                at: git_dir.clone(),
                read_only: false,
                source: Source::Directory,
            },
            hooks: Mount {
                host: git_dir.join("hooks"),
                at: git_dir.join("hooks"),
                read_only: true,
                source: Source::Directory,
            },
            config: Mount {
                host: git_dir.join("config"),
                at: git_dir.join("config"),
                read_only: true,
                source: Source::File,
            },
        }
    }

    /// Every mount, in the spec's order — the order they are passed to `docker run`, and the order a
    /// diagnostic lists them in.
    pub fn all(&self) -> [&Mount; 4] {
        [&self.worktree, &self.git_dir, &self.hooks, &self.config]
    }

    /// The `-v` arguments, two per mount.
    pub fn arguments(&self) -> Vec<String> {
        let mut arguments = Vec::with_capacity(8);
        for mount in self.all() {
            arguments.push("-v".to_string());
            arguments.push(mount.argument());
        }
        arguments
    }

    /// Whether every source is on disk and of the kind its mount needs.
    ///
    /// `docker run -v` creates a missing source as an empty directory, so asking first is the difference
    /// between "the session has no worktree" and a container that quietly mounts nothing. The answer is
    /// the whole list of what is wrong, not the first, because a broken projects root is usually broken
    /// in more than one place and the operator should fix it in one edit.
    pub fn check(&self) -> Result<(), Unmountable> {
        let mut problems = Vec::new();
        for mount in self.all() {
            if !mount.host.exists() {
                problems.push(format!("{} is not there", mount.host.display()));
            } else if !mount.source.holds(&mount.host) {
                problems.push(format!(
                    "{} is not a {}",
                    mount.host.display(),
                    mount.source.describe()
                ));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Unmountable { problems })
        }
    }
}

/// One or more mount sources that are not what the mounts need.
///
/// Named for what it is rather than for the commonest cause: a source can be *missing* or of the *wrong
/// kind* (a `.git` that is a file, a `.git/config` that is a directory), and both make the same four mounts
/// unmountable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmountable {
    problems: Vec<String>,
}

impl fmt::Display for Unmountable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the worktree cannot be mounted: {}",
            self.problems.join("; ")
        )
    }
}

impl std::error::Error for Unmountable {}

/// What one container may use, from the config (the story's "resource limits" line in the spec's
/// *Known and accepted risks* table, which is where the container's limits actually live).
///
/// Three integers rather than a struct of Docker's own strings: `--cpus 2` and `--memory 4096m` are
/// spellings of a number, and a config `Kind` has no decimal — asking an operator to write `"2.0"` would
/// be a string this build then has to parse and refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// `--cpus`. A whole number of CPUs: a fractional share is a tuning decision this system does not
    /// need to express, and Docker accepts the integer form.
    pub cpus: u32,
    /// `--memory`, in mebibytes as Docker spells `m`.
    pub memory_mb: u64,
    /// `--pids-limit`: how many processes the container may have. A fork bomb is a fork bomb even with
    /// `--cap-drop ALL`, and this is the one limit that bounds it.
    pub pids: u64,
}

impl Default for Limits {
    /// The documented defaults (`docs/build.md`'s flag list, and the spec's *Known and accepted risks*
    /// table's "resource limits").
    fn default() -> Self {
        Self {
            cpus: 2,
            memory_mb: 4096,
            pids: 512,
        }
    }
}

/// The machine's container settings: everything a container needs that is not the session itself.
///
/// One value so the backend takes one argument rather than seven, and so there is exactly one place that
/// knows where the projects root, the image, the daemon's address and the limits come from
/// ([`Self::of`]). Tests build one with [`Self::at`], which is the same value with the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSettings {
    /// The machine's projects root: `<projects>/.worktrees/<id>` and `<projects>/<project>` hang off it.
    pub projects_root: PathBuf,
    /// The image a container runs: `agent_image`.
    pub image: String,
    /// The daemon's address, as `docker -H` takes it; `None` is the CLI's own default.
    pub docker_host: Option<String>,
    pub limits: Limits,
}

impl ContainerSettings {
    /// The settings for a machine whose projects root is `projects_root`, everything else default.
    ///
    /// The defaults come from [`Config::default`] rather than from a second copy, so a test that builds a
    /// machine this way is testing the same numbers a served one would use.
    pub fn at(projects_root: impl Into<PathBuf>) -> Self {
        Self::of(&Config::default()).with_projects_root(projects_root)
    }

    /// The same settings on another projects root, for a test that builds its own.
    pub fn with_projects_root(mut self, projects_root: impl Into<PathBuf>) -> Self {
        self.projects_root = projects_root.into();
        self
    }

    /// The machine's settings as the config file spells them.
    pub fn of(config: &Config) -> Self {
        Self {
            projects_root: config.projects_root.clone(),
            image: config.agent_image.clone(),
            docker_host: config.docker_host.clone(),
            limits: Limits {
                cpus: config.container_cpus,
                memory_mb: config.container_memory_mb,
                pids: config.container_pids,
            },
        }
    }

    /// The `docker` CLI pointed at this machine's daemon.
    pub fn docker(&self) -> Docker {
        Docker::new(self.docker_host.clone())
    }
}

/// One session's container: the image it runs, the name it takes, and how it is mounted.
///
/// HS-028 adds the lifecycle flags (`--rm`, `-i`, `--init`), the network and the limits; HS-036 makes the
/// network real. This story's half is the mounts and the hardening, and it is one builder on purpose:
/// a second place that spells `--cap-drop ALL` would be a place to forget it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionContainer {
    /// The image the container runs: `agent_image` (spec item 10 §3 builds it).
    pub image: String,
    /// `s_<id>`: how the container is named, and how HS-029 finds a leftover one.
    pub name: String,
    /// The session id, as `HS_SESSION_ID` carries it and as the name carries it.
    pub session_id: String,
    /// `agents_<id>`: the private network this container joins, and the only one it joins (HS-028).
    pub network: String,
    pub mounts: Mounts,
    pub limits: Limits,
}

impl SessionContainer {
    /// The container for `session`, in the machine `settings` describe.
    ///
    /// The mounts are derived from the session row's project and id, never read back from its
    /// `worktree` column: the naming rule has one home, and a row written by an older binary would
    /// otherwise decide where the container's files are.
    pub fn of(settings: &ContainerSettings, session: &Session) -> Self {
        let worktree = Worktree::of(&settings.projects_root, &session.project, &session.id);
        Self {
            image: settings.image.clone(),
            name: format!("{NAME_PREFIX}{}", session.id),
            session_id: session.id.clone(),
            network: network_name(&session.id),
            mounts: Mounts::of(&worktree),
            limits: settings.limits,
        }
    }

    /// The `docker run` arguments, up to and including the image.
    ///
    /// Everything before the image is a flag, so this is exactly the prefix HS-028 and HS-036 extend: the
    /// image is the first non-flag argument, and whatever runs inside the container follows it.
    ///
    /// `--user` is deliberately absent. The image's own default user is what runs the agent, and the
    /// deploy step (HS-047) is what guarantees that user is the one that owns the host's files. Naming a
    /// uid here would be a second, guessed answer to a question the image already answers — so the
    /// question is *asserted* instead: `id -u` in the container is not `0` (the gate's case checks it).
    pub fn run_args(&self) -> Vec<String> {
        let mut arguments = vec![
            "run".to_string(),
            // `--detach` until HS-031: pi speaks RPC over the container's stdin, and an attached process
            // with piped stdio is that story's change. Everything else in spec item 6's run line is here.
            "--detach".to_string(),
            // The two flags that make a stop leave nothing behind and a killed container not become a
            // zombie: `--rm` is spec item 6's, and `--init` puts a reaping init in as pid 1, so the
            // agent's own children are not the container's problem.
            "--rm".to_string(),
            "--init".to_string(),
            "--name".to_string(),
            self.name.clone(),
            "--network".to_string(),
            self.network.clone(),
            // The four list entries of spec item 1's third bullet that no mount can express: a dropped
            // capability set is what makes remount and unmount impossible, and `no-new-privileges` is
            // what keeps a setuid binary in the worktree from getting one back.
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--workdir".to_string(),
            WORKDIR.to_string(),
            "--env".to_string(),
            format!("{SESSION_ID_ENV}={}", self.session_id),
            // The three limits: a runaway agent is bounded in CPU, memory and processes, so
            // "it cannot take the machine down" is a kernel fact rather than a hope.
            "--cpus".to_string(),
            self.limits.cpus.to_string(),
            "--memory".to_string(),
            format!("{}m", self.limits.memory_mb),
            "--pids-limit".to_string(),
            self.limits.pids.to_string(),
        ];
        arguments.extend(self.mounts.arguments());
        arguments.push(self.image.clone());
        arguments.extend(IDLE_COMMAND.map(str::to_string));
        arguments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Status;

    fn session(project: &str, id: &str) -> Session {
        Session {
            id: id.to_string(),
            project: project.to_string(),
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

    fn settings() -> ContainerSettings {
        ContainerSettings {
            projects_root: PathBuf::from("/srv/holosmith/projects"),
            image: "agent-image:0.1.0".to_string(),
            docker_host: None,
            limits: Limits::default(),
        }
    }

    fn container() -> SessionContainer {
        SessionContainer::of(&settings(), &session("holosmith", "0123456789abcdef"))
    }

    #[test]
    fn the_four_mounts_are_the_specs_four_in_its_order() {
        let mounts = container().mounts;
        let rendered: Vec<String> = mounts.all().iter().map(|mount| mount.argument()).collect();
        assert_eq!(
            rendered,
            [
                "/srv/holosmith/projects/.worktrees/0123456789abcdef:/work",
                "/srv/holosmith/projects/holosmith/.git:/srv/holosmith/projects/holosmith/.git",
                "/srv/holosmith/projects/holosmith/.git/hooks:\
                 /srv/holosmith/projects/holosmith/.git/hooks:ro",
                "/srv/holosmith/projects/holosmith/.git/config:\
                 /srv/holosmith/projects/holosmith/.git/config:ro",
            ]
        );
    }

    #[test]
    fn every_mount_but_the_worktree_keeps_its_host_path_inside_the_container() {
        // The worktree's `.git` file holds an absolute `gitdir:` under the project's repository, so a
        // different container path would point git at something that is not mounted (the module doc).
        let mounts = container().mounts;
        assert_ne!(
            mounts.worktree.host, mounts.worktree.at,
            "/work is the exception"
        );
        for mount in [&mounts.git_dir, &mounts.hooks, &mounts.config] {
            assert_eq!(
                mount.host,
                mount.at,
                "{} moved inside the container",
                mount.host.display()
            );
        }
        assert_eq!(mounts.worktree.at, Path::new(WORKDIR));
    }

    #[test]
    fn exactly_the_hooks_and_the_config_are_read_only() {
        let mounts = container().mounts;
        assert!(!mounts.worktree.read_only);
        assert!(
            !mounts.git_dir.read_only,
            "objects, refs, logs and worktrees/<id> are writable, so `.git` itself must be"
        );
        assert!(
            mounts.hooks.read_only,
            "a hook is a program the host would run"
        );
        assert!(
            mounts.config.read_only,
            "core.fsmonitor and core.sshCommand live here"
        );
    }

    #[test]
    fn the_config_is_a_file_and_the_other_three_are_directories() {
        // A kind the mount does not expect is a `-v` of the wrong thing: Docker would mount a directory
        // where git expects its config file, or the reverse.
        let mounts = container().mounts;
        assert_eq!(mounts.config.source, Source::File);
        for mount in [&mounts.worktree, &mounts.git_dir, &mounts.hooks] {
            assert_eq!(mount.source, Source::Directory);
        }
    }

    #[test]
    fn the_run_arguments_pin_the_name_the_hardening_and_the_session_id() {
        let container = container();
        let arguments = container.run_args();
        assert_eq!(arguments[0], "run");
        for (flag, value) in [
            ("--name", "s_0123456789abcdef"),
            ("--cap-drop", "ALL"),
            ("--security-opt", "no-new-privileges"),
            ("--workdir", WORKDIR),
            ("--env", "HS_SESSION_ID=0123456789abcdef"),
        ] {
            let at = arguments
                .iter()
                .position(|argument| argument == flag)
                .unwrap_or_else(|| panic!("{flag} is missing from {arguments:?}"));
            assert_eq!(arguments[at + 1], value, "{flag}");
        }
        let image = arguments
            .iter()
            .position(|argument| argument == "agent-image:0.1.0")
            .expect("the image is in the run vector");
        assert_eq!(
            &arguments[image + 1..],
            IDLE_COMMAND,
            "the image is followed by the placeholder command until HS-031 runs pi"
        );
        assert!(
            !arguments.iter().any(|argument| argument == "--user"),
            "the image's own user runs the agent; naming a uid here would be a guessed second answer"
        );
    }

    #[test]
    fn a_mount_whose_source_is_not_on_disk_is_reported_rather_than_created() {
        // `docker run -v` creates a missing source as a root-owned empty directory, so a projects root
        // that is not there would give the agent an empty `/work` and no error at all.
        let mounts = Mounts::of(&Worktree::of(
            Path::new("/nonexistent/projects"),
            "holosmith",
            "0123456789abcdef",
        ));
        let error = mounts
            .check()
            .expect_err("nothing is missing only if nothing exists");
        let sentence = error.to_string();
        assert!(
            sentence.starts_with("the worktree cannot be mounted: "),
            "{sentence}"
        );
        for path in [
            "/nonexistent/projects/.worktrees/0123456789abcdef",
            "/nonexistent/projects/holosmith/.git",
            "/nonexistent/projects/holosmith/.git/hooks",
            "/nonexistent/projects/holosmith/.git/config",
        ] {
            assert!(
                sentence.contains(&format!("{path} is not there")),
                "{path} is not named in {sentence}"
            );
        }
    }

    #[test]
    fn a_source_of_the_wrong_kind_is_named_as_the_wrong_kind() {
        // The failure this catches is a repository whose `.git` is a *file* (a submodule or a linked
        // worktree), which HS-026 refuses at the create; the mount check is the second line of defence
        // and says which source it was rather than "not there".
        let dir = crate::container::tests::scratch("wrong-kind");
        let projects = dir.join("projects");
        let repository = projects.join("holosmith");
        std::fs::create_dir_all(repository.join(".git/hooks")).expect("writable");
        std::fs::write(repository.join(".git/config"), "").expect("writable");
        std::fs::create_dir_all(projects.join(".worktrees/0123456789abcdef")).expect("writable");
        // `.git` is right; the worktree directory is a file this time.
        std::fs::remove_dir_all(projects.join(".worktrees/0123456789abcdef")).expect("writable");
        std::fs::write(projects.join(".worktrees/0123456789abcdef"), "").expect("writable");
        let mounts = Mounts::of(&Worktree::of(&projects, "holosmith", "0123456789abcdef"));
        let error = mounts
            .check()
            .expect_err("a file where a directory belongs");
        let sentence = error.to_string();
        assert!(sentence.contains("is not a directory"), "{sentence}");
        assert!(!sentence.contains("is not a file"), "{sentence}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scratch directory outside the repository, removed by the caller.
    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "holosmith-container-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
        path
    }
}
