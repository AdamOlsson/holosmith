use std::process::ExitCode;

use clap::Parser;

use holosmith::cli::{Cli, Cmd, ContainerAction, ProxyAction};
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
            backend,
        }),
        Cmd::Version { json } => commands::version::run(json),
        Cmd::Config { action } => commands::config::run(action, cli.config),
        Cmd::Proxy { which } => match which {
            ProxyAction::Egress { listen, allow } => {
                commands::proxy::run(commands::proxy::ProxyArgs {
                    listen,
                    allow,
                    config: cli.config,
                })
            }
            ProxyAction::Key { listen, keys } => {
                commands::proxy::run_key(commands::proxy::KeyProxyArgs {
                    listen,
                    keys,
                    config: cli.config,
                })
            }
        },
        Cmd::Key { action } => commands::key::run(action, cli.config),
        Cmd::Doctor { full, .. } => commands::doctor::run(commands::doctor::DoctorArgs {
            config: cli.config,
            depth: if full {
                holosmith::doctor::Depth::Full
            } else {
                holosmith::doctor::Depth::Quick
            },
        }),
        Cmd::Container { action } => match action {
            ContainerAction::Snapshot => commands::container::run_snapshot(),
        },
    }
}
