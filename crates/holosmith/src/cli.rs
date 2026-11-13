//! The command line. Phase 1 adds `doctor`, `key set` and the rest of `config`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::runtime::Backend;

#[derive(Debug, Parser)]
#[command(name = "holosmith", version, about = "Run Holosmith")]
pub struct Cli {
    /// Path to the config file (default: /etc/holosmith/config.toml).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run the control listener in the foreground until SIGTERM or SIGINT.
    Serve {
        /// Port for the control listener; the control origin's port follows it.
        #[arg(long, value_name = "PORT")]
        port: Option<u16>,
        /// Data root for this run, instead of the one in the config file.
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
        /// Directory the admin socket (`holosmith drain`) lives in, instead of `/run/holosmith`.
        #[arg(long, value_name = "PATH")]
        admin_dir: Option<PathBuf>,
        /// Which backend runs sessions.
        #[arg(long, value_enum, value_name = "BACKEND")]
        backend: Option<Backend>,
    },
    /// Version, git tag, pinned pi version, schema version.
    Version {
        /// Print one line of JSON instead of `key value` lines.
        #[arg(long)]
        json: bool,
    },
    /// Check, print or edit the machine's config file.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Run one of the proxies, in the foreground. Used inside containers.
    Proxy {
        #[command(subcommand)]
        which: ProxyAction,
    },
    /// Whether this machine is fit to run sessions.
    Doctor {
        /// The list `serve` refuses on, with no container: what the unit file runs as `ExecStartPre`.
        /// This is the default when neither flag is given.
        #[arg(long, conflicts_with = "full")]
        quick: bool,
        /// Everything `--quick` has, plus the network self-test, the proxies and the agent image's pi version.
        #[arg(long)]
        full: bool,
        /// The root to examine instead of `HOLOSMITH_ROOT`'s — the directory the config, the data root, the
        /// release tree and the unit live under.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Store, list or remove the provider keys the key proxy uses.
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    /// Compile a release artifact in the pinned builder container, so nothing is built on this host.
    ///
    /// The host orchestrates Docker and nothing else: no `build.rs`, no proc macro and no toolchain step runs
    /// outside the container. The artifact and its SHA-256 land in `--out`, which is where `holosmith upgrade`
    /// reads them; a failure prints the container's own last lines rather than a generic error.
    Build {
        /// The triple to build (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`). Absent means the
        /// architecture this binary was built for.
        #[arg(long, value_name = "TRIPLE")]
        target: Option<String>,
        /// Where the artifact and its digest land. Absent means `releases/<tag>/` for the tag of the tree being
        /// built (the release tree's layout; the `current` symlink is managed here).
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Promote a new version: the whole pipeline, one printed line per step.
    ///
    /// The review gate acknowledged, the host smoke test, the two images when they changed, the drain, a database
    /// backup, the release directory, the `current` symlink, `systemctl restart` and a 20-second health poll that
    /// puts the previous release back when the candidate does not answer. Every refusal leaves the machine as it
    /// found it, and says so — except that a failure after the drain leaves it draining, which the sentence names.
    Upgrade {
        /// The candidate: the file a promotion is about to make live.
        #[arg(value_name = "BINARY")]
        binary: PathBuf,
        /// Item 12's diff review, acknowledged. A promotion without it is refused: that review is the real gate.
        #[arg(long)]
        acknowledge_review: bool,
        /// How long the drain may wait for the turns that are running (`10m`, `500ms`).
        #[arg(long, value_name = "DURATION", default_value = "10m", value_parser = parse_wait)]
        wait: std::time::Duration,
        /// Promote into this root instead of `HOLOSMITH_ROOT`'s.
        ///
        /// The same meaning as `server deploy --root`: the releases and `current` live under it. The restart is the
        /// one command that is root's, and it goes through the machine's one sudoers line; without that line the
        /// switch happens and this command says the restart is yours.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Go back one release: switch `current` to the release before the live one, drain, restart and verify.
    ///
    /// The emergency brake for a promotion that passed its health check but turned out to be wrong. It drains first,
    /// switches `current` **back one release in the tree's own order**, restarts the unit and polls `/api/health` for
    /// a process younger than that restart. There is no review acknowledgement: the review is a pre-promotion
    /// gate, and a rollback is the command that undoes a promotion. The database is never restored — the migration
    /// rule (expand/contract) is what makes switching back safe.
    Rollback {
        /// The release to switch to, by tag. Absent means the release the live one replaced.
        #[arg(long, value_name = "TAG")]
        to: Option<String>,
        /// How long the drain may wait for the turns that are running (`10m`, `500ms`).
        #[arg(long, value_name = "DURATION", default_value = "10m", value_parser = parse_wait)]
        wait: std::time::Duration,
        /// Roll back into this root instead of `HOLOSMITH_ROOT`'s.
        ///
        /// The same meaning as `upgrade --root`: the releases and `current` live under it.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Container helpers, used inside a session's container.
    Container {
        #[command(subcommand)]
        action: ContainerAction,
    },
    /// Run the smoke run against a candidate binary: a promotion's go/no-go (ADR-0081).
    ///
    /// The promotion's go/no-go: it starts the binary named by `--binary` as a `serve` with a smoke config — its own
    /// port, its own temporary roots, `smoke_` in front of every container and network it names — creates a session,
    /// runs one turn through a container whose `pi` replays a recorded turn, asserts the frames, deletes it, and
    /// answers 0 when the candidate passed, 1 when a measurement failed and 2 when the run could not happen at all.
    Smoke {
        /// The binary to test. Absent means this executable.
        #[arg(long, value_name = "PATH")]
        binary: Option<PathBuf>,
        /// The directory the smoke run's config, data root and scratch repository live under. Absent means
        /// `<data_root>/smoke`, which is the one directory the config already guarantees the daemon can see.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// What this machine is and what it is doing: role, version, uptime, session counts, the proxies, and the
    /// health of the machines registered with it.
    Status,
    /// The server's log from journald, or one session's log (`-f` follows either).
    Logs {
        /// Keep printing what arrives.
        #[arg(short = 'f', long)]
        follow: bool,
        /// One session's `<data>/sessions/<id>/session.log` instead of the unit's journald log.
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// Remove the unit, the two proxies and the machine's networks, keeping the data.
    Uninstall {
        /// Also remove the data root: the database, every session's log and `secrets/`.
        #[arg(long)]
        purge: bool,
        /// Override the refusal to purge while sessions are live. Only meaningful with `--purge`.
        #[arg(long, requires = "purge")]
        force: bool,
        /// Tear down this install tree instead of the system paths, as the invoking user (no sudo).
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Refuse new work and wait for running turns, over the local admin socket.
    ///
    /// One connection, one local unix socket, never the network: the machine answers this from
    /// `/run/holosmith/drain.sock` and nowhere else, and a request that reaches the control origin cannot make it
    /// happen. The exit code is 0 when the machine is quiet and 1 when the wait ran out with a turn still
    /// running — in which case the drain stays on rather than pretending it finished.
    Drain {
        /// How long to wait for the turns that are running (`10m`, `500ms`).
        #[arg(long, value_name = "DURATION", default_value = "10m", value_parser = parse_wait)]
        wait: std::time::Duration,
        /// Directory the admin socket lives in, instead of `/run/holosmith`.
        #[arg(long, value_name = "PATH")]
        admin_dir: Option<PathBuf>,
    },
    /// Install and start a server on this machine.
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    /// Install and start a hub on this machine, and manage the machines it forwards to.
    Hub {
        #[command(subcommand)]
        action: HubAction,
    },
}

/// What `server` does.
#[derive(Debug, Subcommand)]
pub enum ServerAction {
    /// Run the deploy's seven steps in order, each checking the current state first. Re-running is safe.
    ///
    /// On a machine whose config says `hub` this installs the **server half** and leaves the config `both`: a role
    /// is not a reason to refuse a deploy.
    Deploy {
        /// Print each step's plan and change nothing. A broken machine still exits non-zero.
        #[arg(long)]
        dry_run: bool,
        /// Install into this directory instead of the system paths, as the invoking user (no sudo).
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Install on another machine: copy this binary there over ssh, verify the copy, and run the same deploy
        /// there as the service user (`sudo -n -u holosmith`), so the target's tree is written by its owner.
        #[arg(long, value_name = "USER@HOST")]
        ssh: Option<String>,
        /// Send this file instead of the binary this command is running. It must be built for the target's
        /// architecture: a mismatch is refused before anything is copied, and this flag is how a matching build is
        /// supplied.
        #[arg(long, requires = "ssh", value_name = "PATH")]
        binary: Option<PathBuf>,
    },
}

/// What `hub` does: install this machine as a hub, or manage the registry it forwards with.
#[derive(Debug, Subcommand)]
pub enum HubAction {
    /// Steps 1, 2, 5, 6 and 7 of the deploy, each checking the current state first. No image is built and no proxy is
    /// started, because a hub runs no containers. Re-running is safe.
    ///
    /// On a machine whose config says `server` this installs the **hub half** and leaves the config `both`.
    Deploy {
        /// Print each step's plan and change nothing. A broken machine still exits non-zero.
        #[arg(long)]
        dry_run: bool,
        /// Install into this root instead of `HOLOSMITH_ROOT`'s (default `/var/lib/holosmith`).
        ///
        /// The root is one directory: the config, the data root, the release tree and the projects. The deploy
        /// never escalates — what root owns is printed as commands for you to run — so what it needs is a root
        /// this user can write, which is the tree a test or a by-hand install owns.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Install on another machine: copy this binary there over ssh, verify the copy, and run the same deploy
        /// there as the service user (`sudo -n -u holosmith`), so the target's tree is written by its owner.
        #[arg(long, value_name = "USER@HOST")]
        ssh: Option<String>,
        /// Send this file instead of the binary this command is running. It must be built for the target's
        /// architecture: a mismatch is refused before anything is copied, and this flag is how a matching build is
        /// supplied.
        #[arg(long, requires = "ssh", value_name = "PATH")]
        binary: Option<PathBuf>,
    },
    /// Manage this machine's host registry — the machines it forwards to — with the API's own checks.
    Host {
        #[command(subcommand)]
        action: HostAction,
    },
}

/// The registry's three commands. Each one is a call to **this machine's own** `POST`/`GET`/`DELETE /api/hosts`, so
/// the rules, their codes and their sentences are the route's and never a second copy of them.
#[derive(Debug, Subcommand)]
pub enum HostAction {
    /// Register one machine, or update the row it already has (a name is a row's identity).
    Add {
        /// The name the row takes: one path segment — lowercase letters, digits and `-`.
        name: String,
        /// The machine's control origin: `http(s)`, a MagicDNS name or an address in 100.64.0.0/10, and a port.
        url: String,
    },
    /// Print every row, by name.
    List,
    /// Forget one machine. A row that names this machine is refused while it has live sessions.
    Remove {
        /// The row's name.
        name: String,
        /// Remove the row even when it is this machine and it has live sessions (the API's `?force=true`).
        #[arg(long)]
        force: bool,
    },
}

/// The proxies a container runs.
#[derive(Debug, Subcommand)]
pub enum ProxyAction {
    /// The egress proxy: the only route out of a session container.
    Egress {
        /// Address to listen on.
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// A domain a tunnel may target. Repeatable; absent means the config's `[egress] allow`.
        #[arg(long, value_name = "DOMAIN")]
        allow: Vec<String>,
    },
    /// The key proxy: validates a session's dummy key and swaps in the real provider key.
    Key {
        /// Address to listen on.
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// The key file. Absent means the config's `<data>/secrets/keys.toml`.
        #[arg(long, value_name = "PATH")]
        keys: Option<PathBuf>,
    },
}

/// The container helpers.
#[derive(Debug, Subcommand)]
pub enum ContainerAction {
    /// Save the worktree's uncommitted work at `refs/holosmith/wip/<HS_SESSION_ID>`.
    Snapshot,
    /// Be pi, for the smoke run: replay the committed turn on stdin/stdout.
    ///
    /// The smoke image's `/usr/local/bin/pi` is a two-line wrapper around this, so a session's container runs this
    /// binary under pi's own name and `agent::COLUMN`'s launch line, version check and record framing are the real
    /// ones. Every argument after the subcommand is pi's and is **ignored** — `--mode rpc`, `--no-session`,
    /// `--approve`, `--model` and `--thinking` are the launch's, and a fake that refused them would be a fake nobody
    /// can start the way the build starts pi. `--version` answers the pinned version, which is what
    /// `agent::version_of` asks the image.
    FakePi {
        /// pi's own flags, accepted and ignored; `--version` is the one that is read.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "PI_ARGS"
        )]
        pi_args: Vec<String>,
    },
}

/// The three key subcommands.
#[derive(Debug, Subcommand)]
pub enum KeyAction {
    /// Read one provider's key from stdin and store it, mode 0600.
    Set {
        /// The provider id (`anthropic`, `openai`, …).
        provider: String,
    },
    /// Print the providers that have a key. Never the values.
    List,
    /// Remove one provider's key.
    Remove {
        /// The provider id.
        provider: String,
    },
}

/// The three config subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Report every problem in the config file.
    Validate,
    /// Print the resolved config, defaults and all, as TOML.
    Show,
    /// Open the config file in $VISUAL or $EDITOR, then validate what was saved.
    Edit,
}

/// `--wait 10m`: a span of time in the config file's own grammar (`10s`, `1500ms`, `2m`).
///
/// One parser for one idea. The config's [`crate::config::Duration`] already refuses a bare number (its unit
/// would be a guess) and a zero (a wait that waits for nothing is a mistake, not a value), and a second grammar
/// on the command line would be a second set of the same mistakes. `clap` turns the `Err` into a usage error
/// before anything is loaded, which is where a typo in a flag belongs.
fn parse_wait(text: &str) -> Result<std::time::Duration, String> {
    text.parse::<crate::config::Duration>()
        .map(std::time::Duration::from)
}
