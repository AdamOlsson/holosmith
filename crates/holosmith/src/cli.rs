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
