use std::process::ExitCode;

use clap::Parser;

use holosmith::cli::{Cli, Cmd};
use holosmith::commands;
use holosmith::config::Overrides;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve {
            port,
            data_dir,
            backend,
        } => commands::serve::run(commands::serve::ServeArgs {
            config: cli.config,
            overrides: Overrides {
                port,
                data_root: data_dir,
            },
            backend: backend.unwrap_or_default(),
        }),
        Cmd::Version { json } => commands::version::run(json),
        Cmd::Config { action } => commands::config::run(action, cli.config),
    }
}
