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
    /// Container helpers, used inside a session's container.
    Container {
        #[command(subcommand)]
        action: ContainerAction,
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
