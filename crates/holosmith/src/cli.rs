//! The command line. Phase 1 adds `serve`, `doctor`, `key set` and `config validate`.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "holosmith", version, about = "Run Holosmith")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Version, git tag, pinned pi version, schema version.
    Version {
        /// Print one line of JSON instead of `key value` lines.
        #[arg(long)]
        json: bool,
    },
}
