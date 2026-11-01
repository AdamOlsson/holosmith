//! `holosmith version`: four `key value` lines, or one line of JSON.

use std::process::ExitCode;

use crate::version::VersionInfo;

pub fn run(json: bool) -> ExitCode {
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
        Err(error) => {
            eprintln!("holosmith: cannot render version JSON: {error}");
            ExitCode::FAILURE
        }
    }
}
