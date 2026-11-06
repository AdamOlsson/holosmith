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

/// The egress proxy's container (HS-037, spec item 2).
///
/// One per machine, not one per session: it is attached to the machine's `egress` network and, per session,
/// to that session's own network ([`PROXY_ALIAS`] is how a container finds it). The name is item 10's
/// (`hs_egress`) because `uninstall` and `doctor` (HS-045) both have to find it by name.
pub const PROXY_NAME: &str = "hs_egress";

/// The name a session's container resolves the egress proxy by (spec item 2's `http://egress-proxy:3128`).
///
/// A **network alias**, not a container name: the proxy is joined to a session's network with
/// `docker network connect --alias`, and Docker's embedded DNS answers a container's name and its aliases on
/// the networks the asker shares with it. Nothing resolves outside the session's own networks, which is the
/// other half of "resolve nothing externally".
pub const PROXY_ALIAS: &str = "egress-proxy";

/// The variable a session's container finds the proxy in, and the only route out it has (spec item 2).
pub const PROXY_ENV: &str = "HTTPS_PROXY";

/// How long starting the proxy container may take before the boot gives up on it (HS-037).
///
/// A **bound on the startup path**, for the reason [`crate::selftest::PROBE_TIMEOUT`] is one and
/// [`crate::docker`]'s bounded calls exist: a `docker run` that has to pull the image, or a daemon that accepts
/// the request and then wedges, must not keep the control listener from binding — that listener is the one
/// screen an operator needs in order to see what is wrong. Generous, because a first run on a slow machine
/// unpacks an image.
pub const PROXY_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// The value [`PROXY_ENV`] carries for a session: `http://<id>@egress-proxy:3128`.
///
/// The session id is the proxy's **username**, which is how a request is attributed to a session (item 2), and
/// the password is empty — nothing checks it, and a secret in an environment variable is exactly what item 2
/// forbids. `NO_PROXY=key-proxy` ([`crate::agent::ENVIRONMENT`]) keeps a provider call off this route: the key
/// proxy is what makes a model work (HS-039), not this one.
pub fn proxy_url(session_id: &str) -> String {
    format!("http://{session_id}@{PROXY_ALIAS}:{}", crate::egress::PORT)
}

/// The command the container runs is pi's RPC loop (HS-031, [`crate::agent::COMMAND`]); until HS-030 the
/// container was detached on `sleep infinity`, and this constant is what replaced that placeholder.
///
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

/// The four mounts of spec item 1 and the agent directory of spec item 2, in the order the spec lists
/// them.
///
/// A struct rather than a `Vec<Mount>` so a caller cannot drop one: all four git mounts are the isolation,
/// and three of them without the fourth is a weaker thing than the spec describes. The fifth is HS-031's:
/// pi's own directory, which is not an isolation rule but the place pi keeps its configuration.
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
    /// `<data>/sessions/<id>/pi-home` at its own path: pi's configuration, writable (spec item 2).
    ///
    /// Writable because pi writes into it (`auth.json`, `models-store.json` were both created by the first
    /// launch this was measured with), and at its own path rather than the spec's illustrative
    /// `/home/agent/.pi/agent` for the reason ADR-0035 gives about `--user`: the image decides its user, and
    /// Docker creates a missing mount *destination* as a root-owned directory the image's own user cannot
    /// write. `PI_CODING_AGENT_DIR` is the path's other half.
    pub agent: Mount,
}

impl Mounts {
    /// The mounts for the session whose worktree is `worktree` and whose agent directory is `home`.
    ///
    /// The paths are the worktree's own ([`Worktree`]), so the naming rule lives in one place: this
    /// function derives nothing and decides nothing.
    pub fn of(worktree: &Worktree, home: PathBuf) -> Self {
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
            agent: Mount {
                host: home.clone(),
                at: home,
                read_only: false,
                source: Source::Directory,
            },
        }
    }

    /// Every mount, in the spec's order — the order they are passed to `docker run`, and the order a
    /// diagnostic lists them in.
    pub fn all(&self) -> [&Mount; 5] {
        [
            &self.worktree,
            &self.git_dir,
            &self.hooks,
            &self.config,
            &self.agent,
        ]
    }

    /// The `-v` arguments, two per mount.
    pub fn arguments(&self) -> Vec<String> {
        let mut arguments = Vec::with_capacity(10);
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
/// knows where the projects root, the data root, the image, the daemon's address, the limits and the agent's
/// own environment come from ([`Self::of`]). Tests build one with [`Self::at`], which names both roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSettings {
    /// The machine's projects root: `<projects>/.worktrees/<id>` and `<projects>/<project>` hang off it.
    pub projects_root: PathBuf,
    /// The machine's data root: `<data>/sessions/<id>/pi-home` is the agent's directory (HS-031), which is
    /// a mount source, and `<data>/sessions/<id>/session.log` is where the launch line goes.
    ///
    /// It has to be a path the **daemon** can see, because that directory is bind-mounted into the
    /// container: a data root on a tmpfs the daemon does not share gives the agent an empty directory and no
    /// error (the same fact ADR-0035 records for the projects root).
    pub data_root: PathBuf,
    /// The image a container runs: `agent_image`.
    pub image: String,
    /// The daemon's address, as `docker -H` takes it; `None` is the CLI's own default.
    pub docker_host: Option<String>,
    pub limits: Limits,
}

impl ContainerSettings {
    /// The settings for a machine whose two roots are `projects_root` and `data_root`, everything else
    /// default.
    ///
    /// Both roots are arguments rather than defaults: the config's default data root is `/var/lib/holosmith`,
    /// and a test that built a machine from the defaults would create a directory a real deployment owns.
    /// The numbers come from [`Config::default`] rather than from a second copy, so a test that builds a
    /// machine this way is testing the same limits a served one would use.
    pub fn at(projects_root: impl Into<PathBuf>, data_root: impl Into<PathBuf>) -> Self {
        let mut settings = Self::of(&Config::default());
        settings.projects_root = projects_root.into();
        settings.data_root = data_root.into();
        settings
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
            data_root: config.data_root.clone(),
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
/// HS-028 added the lifecycle flags (`--rm`, `-i`, `--init`), the network and the limits; HS-031 turns the
/// container into the agent, so the run vector also carries pi's five environment variables and the
/// command itself. This story's half is one builder on purpose: a second place that spells `--cap-drop ALL`
/// or the launch command would be a place to forget it.
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
    /// `<data>/sessions/<id>/pi-home`: pi's own directory, as `PI_CODING_AGENT_DIR` carries it (HS-031).
    pub home: PathBuf,
    /// The model the row chose, as `--model` carries it (HS-031).
    pub model: String,
    /// The thinking level the row chose, as `--thinking` carries it (HS-031).
    pub thinking: String,
    pub mounts: Mounts,
    pub limits: Limits,
}

impl SessionContainer {
    /// The container for `session`, in the machine `settings` describe.
    ///
    /// The mounts are derived from the session row's project and id, never read back from its
    /// `worktree` column: the naming rule has one home, and a row written by an older binary would
    /// otherwise decide where the container's files are. The agent directory is the one mount the row
    /// does not name at all: it is `<data>/sessions/<id>/pi-home` by construction ([`crate::agent::home`]).
    pub fn of(settings: &ContainerSettings, session: &Session) -> Self {
        let worktree = Worktree::of(&settings.projects_root, &session.project, &session.id);
        let home = crate::agent::home(&settings.data_root, &session.id);
        Self {
            image: settings.image.clone(),
            name: format!("{NAME_PREFIX}{}", session.id),
            session_id: session.id.clone(),
            network: network_name(&session.id),
            home: home.clone(),
            model: session.model.clone(),
            thinking: session.thinking.clone(),
            mounts: Mounts::of(&worktree, home),
            limits: settings.limits,
        }
    }

    /// The `docker run` arguments, from `run` to the last of the agent's own arguments.
    ///
    /// Everything up to the image is a flag, so the image is the first non-flag argument and whatever runs
    /// inside the container follows it. The vector is **attached** (`-i`, no `--detach`): the server holds
    /// this process's stdin and stdout, which is pi's RPC channel, and its exit code, which is what
    /// [`crate::exit`] classifies (HS-031, ADR-0036's "until HS-031").
    ///
    /// `--user` is deliberately absent. The image's own default user is what runs the agent, and the
    /// deploy step (HS-047) is what guarantees that user is the one that owns the host's files. Naming a
    /// uid here would be a second, guessed answer to a question the image already answers — so the
    /// question is *asserted* instead: `id -u` in the container is not `0` (the gate's case checks it).
    pub fn run_args(&self) -> Vec<String> {
        let mut arguments = vec![
            "run".to_string(),
            // `-i` and not `-t`: stdin stays open for the RPC commands, and a terminal would wrap pi's
            // JSONL in escape sequences and redraw it.
            "-i".to_string(),
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
        // pi's environment (spec item 2): its own directory, and the four switches that keep it from
        // refreshing anything, checking for a release, sending telemetry, or sending a provider call
        // through the egress proxy that is not there yet.
        arguments.push("--env".to_string());
        arguments.push(format!("PI_CODING_AGENT_DIR={}", self.home.display()));
        for (name, value) in crate::agent::ENVIRONMENT {
            arguments.push("--env".to_string());
            arguments.push(format!("{name}={value}"));
        }
        // The only route out (spec item 2): the proxy's address and this session's id as its username. Item 2's
        // environment is advisory, which is why the enforcement is `--internal` plus the proxy's own attach —
        // but a client that does not *know* the proxy's address reaches nothing at all, so this is what makes
        // an allow-listed download work while a direct one fails.
        arguments.push("--env".to_string());
        arguments.push(format!("{PROXY_ENV}={}", proxy_url(&self.session_id)));
        arguments.extend(self.mounts.arguments());
        arguments.push(self.image.clone());
        arguments.extend(crate::agent::COMMAND.map(str::to_string));
        // The row's own two choices, as flags rather than only as the seeded settings file: measured in
        // `pi-agent:local` with `PI_OFFLINE=1`, `get_state` answers `model.id: "unknown"` and
        // `thinkingLevel: "off"` when neither is on the command line, and the real values when they are.
        arguments.push("--model".to_string());
        arguments.push(self.model.clone());
        arguments.push("--thinking".to_string());
        arguments.push(self.thinking.clone());
        arguments
    }
}

/// The egress proxy's container: the machine's one route out, as a `docker run` vector (HS-037, spec item 2).
///
/// Three things about it are load-bearing and easy to lose:
///
/// * **It is not per session.** One container, attached to the machine's `egress` network and, per session,
///   to that session's own network — the second half of "a container's only route out".
///   [`crate::docker::Docker::join_network`] is what joins them, and the network's *alias* is what makes
///   `egress-proxy` resolve.
/// * **It is detached with a restart policy**, unlike a session's container: it has no stdio to hold and no
///   exit to classify, and `unless-stopped` is the spec's own word. `--rm` and `--restart` conflict, so a
///   restart policy means a stopped proxy leaves its name behind — the next start's `rm -f` is what clears it.
/// * **It runs this binary**, mounted read-only at its own host path (item 1's identity rule). The image's
///   own base is irrelevant because the artifact is static; HS-048 ships `holosmith-proxy:<version>` and this
///   mount goes away, which is the one thing [`EgressProxy::of`] decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressProxy {
    /// The image the container runs, from `agent_image` until HS-048 builds a proxy image.
    pub image: String,
    /// This build's binary: both what is mounted and what is run.
    pub binary: PathBuf,
    /// The domains a tunnel may target, from `[egress] allow`.
    pub allow: Vec<String>,
}

impl EgressProxy {
    /// The machine's proxy, or the sentence explaining why this build cannot start one.
    ///
    /// Built from the **config**, not from [`ContainerSettings`]: the proxy is the machine's container, not a
    /// session's — it has no session, no mounts and no limits of its own — so the three things it needs (the
    /// image, the allow-list and this binary's own path) are the machine's settings verbatim.
    ///
    /// The only refusal is "this process does not know its own path": every other input has a default, and a
    /// missing image or a missing daemon is the daemon's answer to `docker run`, which the caller logs.
    pub fn of(config: &Config) -> Result<Self, String> {
        let Some(binary) = std::env::current_exe().ok() else {
            return Err(
                "the egress proxy runs this binary, and this process could not name its own executable, so \
                 no proxy container was started"
                    .to_string(),
            );
        };
        Ok(Self {
            image: config.agent_image.clone(),
            binary,
            allow: config.egress.allow.clone(),
        })
    }

    /// The `docker run` arguments, from `run` to the last `--allow`.
    ///
    /// `--entrypoint` rather than relying on the image's: the image is a session's image (a shell, pi, git),
    /// and its entrypoint is whatever that base does. `--cap-drop ALL` and `no-new-privileges` are here for the
    /// same reason they are on a session's container — a proxy that can open a raw socket is a proxy that is
    /// not a proxy — and it needs no mount of the data root, the projects root or the daemon's socket.
    pub fn run_args(&self) -> Vec<String> {
        let binary = self.binary.display().to_string();
        let mut arguments = vec![
            "run".to_string(),
            // Detached: nothing reads this container's stdio. `--rm` cannot be passed with `--restart`.
            "--detach".to_string(),
            "--name".to_string(),
            PROXY_NAME.to_string(),
            "--network".to_string(),
            crate::docker::EGRESS_NETWORK.to_string(),
            "--restart".to_string(),
            "unless-stopped".to_string(),
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            // The binary, at its own host path, read-only: the container runs the artifact the server is.
            "-v".to_string(),
            format!("{binary}:{binary}:ro"),
            "--entrypoint".to_string(),
            binary,
            self.image.clone(),
            "proxy".to_string(),
            "egress".to_string(),
            "--listen".to_string(),
            format!("0.0.0.0:{}", crate::egress::PORT),
        ];
        for domain in &self.allow {
            arguments.push("--allow".to_string());
            arguments.push(domain.clone());
        }
        arguments
    }
}

/// The machine's key proxy container (HS-039, spec item 2): one `hs_key`, on `egress`, joined to every
/// session network under the alias `key-proxy`.
///
/// It is the same shape as [`EgressProxy`] with two differences that are the story:
///
/// * **It mounts the key store's directory**, read-only, at its own host path: the file inside is written by
///   rename, so only a directory mount sees a new `keys.toml` ([`KeyProxy::mount`]). A session's container
///   mounts neither the directory nor the file: its `models.json` has a dummy key and no real one.
/// * **It accepts a plaintext request from pi and speaks TLS to the provider.** The TLS client is `openssl`
///   from the agent image; the artifact is static and has no TLS stack of its own. That is recorded in
///   ADR-0048 rather than hidden in a dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyProxy {
    /// The image the container runs, from `agent_image` until a proxy image is built.
    pub image: String,
    /// This build's binary: both what is mounted and what is run.
    pub binary: PathBuf,
    /// `<data>/secrets/keys.toml`, mounted read-only. The only container that ever sees a real key.
    pub keys: PathBuf,
}

impl KeyProxy {
    /// The machine's key proxy, or the sentence explaining why this build cannot start one.
    pub fn of(config: &Config) -> Result<Self, String> {
        let Some(binary) = std::env::current_exe().ok() else {
            return Err(
                "the key proxy runs this binary, and this process could not name its own executable, so no proxy container was started"
                    .to_string(),
            );
        };
        Ok(Self {
            image: config.agent_image.clone(),
            binary,
            keys: config.secrets_path(),
        })
    }

    /// The directory the proxy container mounts: `keys`'s parent.
    ///
    /// **The directory, not the file.** [`crate::key::Store`] writes `keys.toml` by temp-file-and-rename,
    /// because the file must never be observable half-written; a *file* bind mount pins the inode, so a
    /// container started before the first create would go on reading the file as it was at mount time. Every
    /// session's dummy is written by that rename, so a file mount makes the proxy answer `bad-key` for every
    /// session that was created after it started — which is all of them. HS-040's V3 run measured exactly that
    /// (`session=- … status=bad-key`); a directory mount resolves `keys.toml` at read time, so a rename inside
    /// it is visible.
    pub fn mount(&self) -> PathBuf {
        self.keys
            .parent()
            .map_or_else(|| self.keys.clone(), Path::to_path_buf)
    }

    /// The `docker run` arguments, from `run` to the last `--keys`.
    pub fn run_args(&self) -> Vec<String> {
        let binary = self.binary.display().to_string();
        let keys = self.keys.display().to_string();
        let mount = self.mount().display().to_string();
        vec![
            "run".to_string(),
            "--detach".to_string(),
            "--name".to_string(),
            crate::key::KEY_PROXY_NAME.to_string(),
            "--network".to_string(),
            crate::docker::EGRESS_NETWORK.to_string(),
            "--network-alias".to_string(),
            crate::key::KEY_PROXY_ALIAS.to_string(),
            "--restart".to_string(),
            "unless-stopped".to_string(),
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "-v".to_string(),
            format!("{binary}:{binary}:ro"),
            // The only mount of the key store anywhere in this build, and a directory for `mount`'s reason. A
            // session's container never names it.
            "-v".to_string(),
            format!("{mount}:{mount}:ro"),
            "--entrypoint".to_string(),
            binary,
            self.image.clone(),
            "proxy".to_string(),
            "key".to_string(),
            "--listen".to_string(),
            format!("0.0.0.0:{}", crate::key::KEY_PROXY_PORT),
            "--keys".to_string(),
            keys,
        ]
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
            data_root: PathBuf::from("/var/lib/holosmith"),
            image: "agent-image:0.1.0".to_string(),
            docker_host: None,
            limits: Limits::default(),
        }
    }

    fn container() -> SessionContainer {
        SessionContainer::of(&settings(), &session("holosmith", "0123456789abcdef"))
    }

    /// The five mounts: spec item 1's four, then the agent's own directory (spec item 2).
    #[test]
    fn the_four_git_mounts_and_the_agent_directory_are_the_specs_five() {
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
                "/var/lib/holosmith/sessions/0123456789abcdef/pi-home:\
                 /var/lib/holosmith/sessions/0123456789abcdef/pi-home",
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
        assert!(
            !arguments.iter().any(|argument| argument == "--user"),
            "the image's own user runs the agent; naming a uid here would be a guessed second answer"
        );
    }

    /// The container is attached and runs pi (HS-031): no `--detach`, one `-i`, and the agent's command.
    #[test]
    fn the_container_is_attached_and_runs_pi_in_rpc_mode() {
        let container = container();
        let arguments = container.run_args();
        assert!(
            arguments.contains(&"-i".to_string()),
            "stdin stays open for the RPC commands: {arguments:?}"
        );
        assert!(
            !arguments.contains(&"--detach".to_string()),
            "a detached container has no stdio to talk to pi over: {arguments:?}"
        );
        let image = arguments
            .iter()
            .position(|argument| argument == "agent-image:0.1.0")
            .expect("the image is in the run vector");
        let launched = &arguments[image + 1..];
        assert_eq!(
            launched,
            [
                "pi",
                "--mode",
                "rpc",
                "--no-session",
                "--approve",
                "--model",
                "claude-sonnet-4.5",
                "--thinking",
                "high",
            ],
            "spec item 2's launch line, with the row's own model and thinking level"
        );
    }

    /// The five launch variables of spec item 2: the agent's directory and the four switches.
    #[test]
    fn the_launch_environment_is_the_agent_directory_and_four_switches() {
        let container = container();
        let arguments = container.run_args();
        let mut environment: Vec<String> = Vec::new();
        for (index, argument) in arguments.iter().enumerate() {
            if argument == "--env" {
                environment.push(arguments[index + 1].clone());
            }
        }
        assert_eq!(
            environment,
            [
                "HS_SESSION_ID=0123456789abcdef".to_string(),
                "PI_CODING_AGENT_DIR=/var/lib/holosmith/sessions/0123456789abcdef/pi-home"
                    .to_string(),
                "NO_PROXY=key-proxy".to_string(),
                "PI_OFFLINE=1".to_string(),
                "PI_SKIP_VERSION_CHECK=1".to_string(),
                "PI_TELEMETRY=0".to_string(),
                // The only route out, with this session's id as the proxy's username (HS-037, item 2).
                "HTTPS_PROXY=http://0123456789abcdef@egress-proxy:3128".to_string(),
            ]
        );
    }

    #[test]
    fn the_proxy_url_is_the_sessions_id_as_the_username_and_no_secret() {
        // The acceptance's two bullets reconcile here: the proxy's address is `egress-proxy:3128`, and the
        // *username* is the session id — which is what `Proxy-Authorization` carries and what the proxy's log
        // line attributes a request to. The password is empty on purpose: nothing checks it, and a secret in an
        // environment variable is what item 2 forbids.
        assert_eq!(
            proxy_url("0123456789abcdef"),
            "http://0123456789abcdef@egress-proxy:3128"
        );
        assert_eq!(PROXY_ALIAS, "egress-proxy");
        assert_eq!(crate::egress::PORT, 3128);
        assert!(
            !proxy_url("x").trim_start_matches("http://").contains(":@"),
            "an empty password would be a colon; there is no password at all"
        );
    }

    #[test]
    fn the_proxy_container_is_detached_restarting_and_lives_on_the_egress_network() {
        let proxy = EgressProxy {
            image: "agent-image:0.1.0".to_string(),
            binary: PathBuf::from("/usr/local/bin/holosmith"),
            allow: vec!["crates.io".to_string(), "static.crates.io".to_string()],
        };
        let arguments = proxy.run_args();
        assert_eq!(arguments[0], "run");
        assert!(
            arguments.contains(&"--detach".to_string()),
            "nothing reads the proxy's stdio: {arguments:?}"
        );
        assert!(
            !arguments.contains(&"--rm".to_string()),
            "`--rm` and `--restart` conflict, so a restart policy rules it out: {arguments:?}"
        );
        let flag = |name: &str| -> String {
            arguments[arguments.iter().position(|a| a == name).expect(name) + 1].clone()
        };
        assert_eq!(flag("--name"), PROXY_NAME);
        assert_eq!(flag("--network"), crate::docker::EGRESS_NETWORK);
        assert_eq!(
            flag("--restart"),
            "unless-stopped",
            "spec item 10's deploy step"
        );
        assert_eq!(flag("--cap-drop"), "ALL");
        assert_eq!(flag("--security-opt"), "no-new-privileges");
        // The binary is mounted read-only at its own host path and is the entrypoint: one artifact, no second
        // program inside the image (ADR-0045).
        assert_eq!(
            flag("-v"),
            "/usr/local/bin/holosmith:/usr/local/bin/holosmith:ro"
        );
        assert_eq!(flag("--entrypoint"), "/usr/local/bin/holosmith");
        // The command is the proxy's own subcommand, with the allow-list as arguments rather than as a mount
        // the container would have to read.
        let image = arguments
            .iter()
            .position(|argument| argument == "agent-image:0.1.0")
            .expect("the image is in the run vector");
        assert_eq!(
            &arguments[image + 1..],
            [
                "proxy",
                "egress",
                "--listen",
                "0.0.0.0:3128",
                "--allow",
                "crates.io",
                "--allow",
                "static.crates.io",
            ]
        );
        // No mount of the data root, the projects root or the daemon's socket: the proxy reads nothing and
        // writes nothing, so there is nothing for it to reach.
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| *argument == "-v")
                .count(),
            1,
            "{arguments:?}"
        );
    }

    #[test]
    fn the_key_proxy_mounts_the_binary_and_the_directory_the_key_file_lives_in() {
        let proxy = KeyProxy {
            image: "agent-image:0.1.0".to_string(),
            binary: PathBuf::from("/usr/local/bin/holosmith"),
            keys: PathBuf::from("/var/lib/holosmith/secrets/keys.toml"),
        };
        let arguments = proxy.run_args();
        let flag = |name: &str| -> String {
            arguments[arguments.iter().position(|a| a == name).expect(name) + 1].clone()
        };
        assert_eq!(arguments[0], "run");
        assert!(arguments.contains(&"--detach".to_string()));
        assert_eq!(flag("--name"), crate::key::KEY_PROXY_NAME);
        assert_eq!(flag("--network"), crate::docker::EGRESS_NETWORK);
        assert_eq!(flag("--network-alias"), crate::key::KEY_PROXY_ALIAS);
        assert_eq!(flag("--restart"), "unless-stopped");
        assert_eq!(flag("--cap-drop"), "ALL");
        assert_eq!(flag("--security-opt"), "no-new-privileges");
        assert_eq!(
            flag("-v"),
            "/usr/local/bin/holosmith:/usr/local/bin/holosmith:ro"
        );
        // Very deliberately two: the binary and the key file. A third would be a data root or a project, and
        // this container has no business reading either.
        let mounts: Vec<&String> = arguments
            .iter()
            .enumerate()
            .filter(|(index, argument)| {
                *argument == "-v"
                    || arguments
                        .get(index + 1)
                        .is_some_and(|next| next == *argument)
            })
            .map(|(_, argument)| argument)
            .collect();
        assert_eq!(mounts.len(), 2, "{arguments:?}");
        assert!(
            arguments
                .iter()
                .any(|argument| argument
                    == "/var/lib/holosmith/secrets:/var/lib/holosmith/secrets:ro"),
            "the directory the key file lives in is mounted read-only at its own path: {arguments:?}"
        );
        // The file itself must not be a mount source: `Store` writes it by rename, and a file bind mount pins
        // the inode, so the proxy would keep reading the store as it was when the container started — every
        // session's dummy would be unknown (HS-040's V3 run measured `session=- … status=bad-key`).
        assert!(
            !arguments.iter().any(|argument| argument
                == "/var/lib/holosmith/secrets/keys.toml:/var/lib/holosmith/secrets/keys.toml:ro"),
            "the key file is mounted as a file, which a rename cannot reach: {arguments:?}"
        );
        assert_eq!(
            flag("--keys"),
            "/var/lib/holosmith/secrets/keys.toml",
            "the proxy still names the file inside the mounted directory"
        );
        let image = arguments
            .iter()
            .position(|argument| argument == "agent-image:0.1.0")
            .expect("the image is in the run vector");
        assert_eq!(
            &arguments[image + 1..],
            [
                "proxy",
                "key",
                "--listen",
                "0.0.0.0:8080",
                "--keys",
                "/var/lib/holosmith/secrets/keys.toml",
            ]
        );
    }

    #[test]
    fn the_proxy_takes_the_machines_image_and_allow_list_and_this_binarys_own_path() {
        // Built from the config rather than from `ContainerSettings`: the proxy has no session, so it has no
        // mounts and no limits, and the binary it runs is *this* one (ADR-0045). `current_exe` answers for the
        // test binary here, which is all this needs — the path is what is mounted, not what is executed here.
        let config = Config {
            agent_image: "agent-image:0.1.0".to_string(),
            egress: crate::config::Egress {
                allow: vec!["crates.io".to_string()],
            },
            ..Config::default()
        };
        let proxy = EgressProxy::of(&config).expect("this process knows its own path");
        assert_eq!(proxy.image, "agent-image:0.1.0");
        assert_eq!(proxy.allow, ["crates.io"]);
        assert_eq!(
            Some(proxy.binary.clone()),
            std::env::current_exe().ok(),
            "the binary is this process's, not a guess at an install path"
        );
    }

    #[test]
    fn a_mount_whose_source_is_not_on_disk_is_reported_rather_than_created() {
        // `docker run -v` creates a missing source as a root-owned empty directory, so a projects root
        // that is not there would give the agent an empty `/work` and no error at all.
        let mounts = Mounts::of(
            &Worktree::of(
                Path::new("/nonexistent/projects"),
                "holosmith",
                "0123456789abcdef",
            ),
            PathBuf::from("/nonexistent/data/sessions/0123456789abcdef/pi-home"),
        );
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
            "/nonexistent/data/sessions/0123456789abcdef/pi-home",
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
        std::fs::create_dir_all(dir.join("data/sessions/0123456789abcdef/pi-home"))
            .expect("writable");
        let mounts = Mounts::of(
            &Worktree::of(&projects, "holosmith", "0123456789abcdef"),
            dir.join("data/sessions/0123456789abcdef/pi-home"),
        );
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
