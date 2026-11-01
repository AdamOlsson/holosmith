use std::process::ExitCode;

use clap::Parser;

use holosmith::cli::{Cli, Cmd};
use holosmith::version::VersionInfo;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Version { json } => {
            let info = VersionInfo::current();
            if !json {
                print!("{}", info.render_human());
                return ExitCode::SUCCESS;
            }
            match info.render_json() {
                Ok(line) => {
                    println!("{line}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("holosmith: cannot render version JSON: {err}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
