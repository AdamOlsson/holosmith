//! `holosmith config validate`, `show` and `edit`.
//!
//! Config problems go to stderr in file order with a count; only a clean run writes to
//! stdout, and it writes the whole document at once, so stdout never holds half a file. That
//! alone does not make `config show > config.toml` safe — the shell truncates the target
//! *before* this binary runs — so write beside the file and move it into place:
//! `config show > config.toml.tmp && mv config.toml.tmp config.toml`. Tool errors keep the
//! `holosmith: ` prefix and the command that failed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cli::ConfigAction;
use crate::config::{
    Config, ConfigPath, ConfigSource, Editor, LoadError, LoadedConfig, Problems, render_show,
    validate_text,
};

pub fn run(action: ConfigAction, named: Option<PathBuf>) -> ExitCode {
    match action {
        ConfigAction::Validate => validate(named),
        ConfigAction::Show => show(named),
        ConfigAction::Edit => edit(named),
    }
}

fn validate(named: Option<PathBuf>) -> ExitCode {
    match Config::load(named) {
        Ok(loaded) => {
            match loaded.source {
                ConfigSource::File => println!("config valid: {}", loaded.path.display()),
                ConfigSource::Defaults => {
                    println!(
                        "config valid: {} (no file, defaults)",
                        loaded.path.display()
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(LoadError::Invalid(problems)) => report(&problems),
        Err(error) => tool_error("validate", error),
    }
}

fn show(named: Option<PathBuf>) -> ExitCode {
    match Config::load(named) {
        Ok(loaded) => match render_show(&loaded) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => tool_error("show", error),
        },
        Err(LoadError::Invalid(problems)) => report(&problems),
        Err(error) => tool_error("show", error),
    }
}

fn edit(named: Option<PathBuf>) -> ExitCode {
    // The editor first: with none set there is no point writing a file the operator cannot
    // open. Then create a missing file (either origin) and let the editor own it.
    let editor = match Editor::from_env() {
        Ok(editor) => editor,
        Err(error) => return tool_error("edit", error),
    };
    let path = ConfigPath::resolve(named);
    if !path.path.exists()
        && let Err(detail) = create(&path.path)
    {
        return tool_error("edit", detail);
    }
    match editor.run(&path.path) {
        Ok(0) => {}
        Ok(code) => {
            // `1` for every failure this command can have, so `2` stays clap's usage code:
            // `vim` exits 2 for a failed save, and a failed save is not a usage error. The
            // editor's own number is in the message instead.
            eprintln!("holosmith: config edit: editor exited with status {code}");
            return ExitCode::FAILURE;
        }
        Err(error) => return tool_error("edit", error),
    }
    // Re-read what was saved. The story never reverts or reformats the operator's file:
    // a save that does not validate is reported as it stands.
    match fs::read_to_string(&path.path) {
        Ok(text) => match validate_text(&path.path, &text) {
            Ok(_) => {
                println!("config valid: {}", path.path.display());
                ExitCode::SUCCESS
            }
            Err(problems) => report(&problems),
        },
        Err(error) => tool_error(
            "edit",
            format!("cannot read {}: {error}", path.path.display()),
        ),
    }
}

/// Write the resolved defaults where the editor expects a file, with the header comments
/// `config show` prints. Never creates a directory.
fn create(path: &Path) -> Result<(), String> {
    if let Some(directory) = path.parent()
        && !directory.as_os_str().is_empty()
        && !directory.is_dir()
    {
        return Err(format!("no such directory: {}", directory.display()));
    }
    let text = render_show(&LoadedConfig::defaults(path)).map_err(|error| error.to_string())?;
    fs::write(path, text).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn report(problems: &Problems) -> ExitCode {
    eprint!("{problems}");
    ExitCode::FAILURE
}

fn tool_error(action: &str, detail: impl std::fmt::Display) -> ExitCode {
    eprintln!("holosmith: config {action}: {detail}");
    ExitCode::FAILURE
}
