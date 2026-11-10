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
    /// Whether this machine is fit to run sessions (spec item 10 §1 and §7).
    Doctor {
        /// The list `serve` refuses on, with no container: what the unit file runs as `ExecStartPre`.
        /// This is the default when neither flag is given.
        #[arg(long, conflicts_with = "full")]
        quick: bool,
        /// Everything `--quick` has, plus the network self-test, the proxies and the agent image's pi version.
        #[arg(long)]
        full: bool,
    },
    /// Store, list or remove the provider keys the key proxy uses.
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    /// Compile a release artifact in the pinned builder container, so nothing is built on this host (spec item
    /// 10's first step).
    ///
    /// The host orchestrates Docker and nothing else: no `build.rs`, no proc macro and no toolchain step runs
    /// outside the container. The artifact and its SHA-256 land in `--out`, which is where `holosmith upgrade`
    /// reads them (HS-078); a failure prints the container's own last lines rather than a generic error.
    Build {
        /// The triple to build (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`). Absent means the
        /// architecture this binary was built for.
        #[arg(long, value_name = "TRIPLE")]
        target: Option<String>,
        /// Where the artifact and its digest land. Absent means `releases/<tag>/` for the tag of the tree being
        /// built (spec item 10's layout; HS-080 owns the `current` symlink).
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Container helpers, used inside a session's container.
    Container {
        #[command(subcommand)]
        action: ContainerAction,
    },
    /// What this machine is and what it is doing: role, version, uptime, session counts, the proxies, and the
    /// health of the machines registered with it (spec's Commands table).
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
    /// Remove the unit, the two proxies and the machine's networks, keeping the data (spec's Commands table).
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
    /// Refuse new work and wait for running turns, over the local admin socket (spec item 10 step 3).
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
    /// Install and start a server on this machine (spec item 10's seven steps).
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    /// Install and start a hub on this machine, and manage the machines it forwards to (spec's Commands table).
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
    /// is not a reason to refuse a deploy (the spec's Roles table, HS-072).
    Deploy {
        /// Print each step's plan and change nothing. A broken machine still exits non-zero.
        #[arg(long)]
        dry_run: bool,
        /// Install into this directory instead of the system paths, as the invoking user (no sudo).
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Install on another machine: copy this binary there over ssh, verify the copy, and run the same deploy
        /// there with sudo (`user@host`).
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
        /// Install into this directory instead of the system paths, as the invoking user (no sudo).
        ///
        /// The spec's Commands table lists this flag for `server deploy` only. It is here with the same meaning and
        /// for the same reason (ADR-0056): an install tree is how the whole deploy is driven by an unprivileged
        /// user, and it is how this story's own acceptance is measured.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Install on another machine: copy this binary there over ssh, verify the copy, and run the same deploy
        /// there with sudo (`user@host`).
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

/// The proxies a container runs (spec item 2's "internal subcommands").
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

/// The container helpers (spec item 9's `hs-snapshot` logic).
#[derive(Debug, Subcommand)]
pub enum ContainerAction {
    /// Save the worktree's uncommitted work at `refs/holosmith/wip/<HS_SESSION_ID>`.
    Snapshot,
}

/// The three key subcommands (spec CLI phase 1).
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
