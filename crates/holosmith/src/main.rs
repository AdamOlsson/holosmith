use std::process::ExitCode;

use clap::Parser;

use holosmith::cli::{Cli, Cmd, ContainerAction, HubAction, ProxyAction, ServerAction};
use holosmith::commands;
use holosmith::config::Overrides;
use holosmith::identity;

/// What was typed: every argument after the program name, joined by single spaces (ADR-0090).
///
/// It is what a refusal carries, because the sentence is about the command rather than about the mistake: a
/// `sudo holosmith status` with no `holosmith status` in the answer leaves the operator to retype from memory. No
/// verb, subcommand or flag is read here — the words are passed through, so the refusal cannot depend on one. They
/// are read as `OsString`s and made lossy here, so a path that is not UTF-8 reaches this sentence rather than
/// panicking on it.
fn typed() -> String {
    std::env::args_os()
        .skip(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() -> ExitCode {
    // Before `Cli::parse`, so no verb and no flag can be read first: uid 0 is refused whatever was typed, which is
    // what keeps a verb from acquiring an exemption (ADR-0090).
    if let Some(refusal) = identity::euid().and_then(|uid| identity::root_refusal(uid, &typed())) {
        eprintln!("{refusal}");
        return ExitCode::FAILURE;
    }
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
                commands::proxy::run_key(commands::proxy::KeyedProxyArgs {
                    listen,
                    keys,
                    config: cli.config,
                })
            }
            ProxyAction::Mcp { listen, keys } => {
                commands::proxy::run_mcp(commands::proxy::KeyedProxyArgs {
                    listen,
                    keys,
                    config: cli.config,
                })
            }
        },
        Cmd::Key { action } => commands::key::run(action, cli.config),
        Cmd::Doctor { full, root, .. } => commands::doctor::run(commands::doctor::DoctorArgs {
            config: cli.config,
            depth: if full {
                holosmith::doctor::Depth::Full
            } else {
                holosmith::doctor::Depth::Quick
            },
            root,
        }),
        Cmd::Update { report, wait, root } => commands::update::run(commands::update::UpdateArgs {
            config: cli.config,
            report,
            wait,
            root,
        }),
        Cmd::Upgrade {
            binary,
            acknowledge_review,
            wait,
            root,
        } => commands::upgrade::run(commands::upgrade::UpgradeArgs {
            config: cli.config,
            binary,
            acknowledge_review,
            wait,
            root,
        }),
        Cmd::Rollback { to, wait, root } => {
            commands::rollback::run(commands::rollback::RollbackArgs {
                config: cli.config,
                to,
                wait,
                root,
            })
        }
        Cmd::Container { action } => match action {
            ContainerAction::Snapshot => commands::container::run_snapshot(),
            ContainerAction::FakePi { pi_args } => commands::container::run_fake_pi(&pi_args),
        },
        Cmd::Smoke { binary, root } => commands::smoke::run(commands::smoke::SmokeArgs {
            config: cli.config,
            binary,
            root,
        }),
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
        Cmd::Install {
            dry_run,
            root,
            verbose,
        } => commands::install::run(commands::install::InstallArgs {
            config: cli.config,
            root,
            dry_run,
            verbose,
        }),
        Cmd::Server { action } => match action {
            ServerAction::Deploy {
                dry_run,
                root,
                ssh,
                binary,
                verbose,
            } => commands::deploy::run(commands::deploy::DeployArgs {
                config: cli.config,
                kind: holosmith::deploy::Kind::Server,
                dry_run,
                root,
                ssh,
                binary,
                verbose,
            }),
        },
        Cmd::Hub { action } => match action {
            HubAction::Deploy {
                dry_run,
                root,
                ssh,
                binary,
                verbose,
            } => commands::deploy::run(commands::deploy::DeployArgs {
                config: cli.config,
                kind: holosmith::deploy::Kind::Hub,
                dry_run,
                root,
                ssh,
                binary,
                verbose,
            }),
            HubAction::Host { action } => commands::hub::run_host(action, cli.config),
        },
        Cmd::Project { action } => commands::project::run(action, cli.config),
    }
}
