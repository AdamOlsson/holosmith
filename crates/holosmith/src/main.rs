use std::process::ExitCode;

use clap::Parser;

use holosmith::cli::{Cli, Cmd, ContainerAction, HubAction, ProxyAction, ServerAction};
use holosmith::commands;
use holosmith::config::Overrides;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve {
            port,
            data_dir,
            admin_dir,
            backend,
        } => commands::serve::run(commands::serve::ServeArgs {
            config: cli.config,
            overrides: Overrides {
                port,
                data_root: data_dir,
            },
            admin_dir,
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
        Cmd::Status => commands::status::run(commands::status::StatusArgs { config: cli.config }),
        Cmd::Logs { follow, session } => commands::logs::run(commands::logs::LogsArgs {
            config: cli.config,
            follow,
            session,
        }),
        Cmd::Uninstall { purge, force, root } => {
            commands::uninstall::run(commands::uninstall::UninstallArgs {
                config: cli.config,
                root,
                purge,
                force,
            })
        }
        Cmd::Drain { wait, admin_dir } => {
            commands::drain::run(commands::drain::DrainArgs { wait, admin_dir })
        }
        Cmd::Server { action } => match action {
            ServerAction::Deploy { dry_run, root } => {
                commands::deploy::run(commands::deploy::DeployArgs {
                    config: cli.config,
                    kind: holosmith::deploy::Kind::Server,
                    dry_run,
                    root,
                })
            }
        },
        Cmd::Hub { action } => match action {
            HubAction::Deploy { dry_run, root } => {
                commands::hub::run_deploy(commands::deploy::DeployArgs {
                    config: cli.config,
                    kind: holosmith::deploy::Kind::Hub,
                    dry_run,
                    root,
                })
            }
            HubAction::Host { action } => commands::hub::run_host(action, cli.config),
        },
    }
}
