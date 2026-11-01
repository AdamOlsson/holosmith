use std::process::ExitCode;

use clap::Parser;

use holosmith::cli::{Cli, Cmd};
use holosmith::commands;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Version { json } => commands::version::run(json),
        Cmd::Config { action } => commands::config::run(action, cli.config),
    }
}
