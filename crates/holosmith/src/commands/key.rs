//! `holosmith key set|list|remove` (spec CLI phase 1).
//!
//! `set` reads the key from **stdin only**. A key in `argv` is a key in the process list, in the shell's
//! history and in any crash report; the spec says stdin, so there is no `--key` flag to get it wrong with.
//! `list` prints provider ids and never values. `remove` is the other half of a rotation.

use std::io::Read;
use std::process::ExitCode;

use crate::cli::KeyAction;
use crate::config::{Config, LoadError};
use crate::key::Store;

pub fn run(action: KeyAction, named: Option<std::path::PathBuf>) -> ExitCode {
    let config = match Config::load(named) {
        Ok(loaded) => loaded.config,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: key: {error}");
            return ExitCode::FAILURE;
        }
    };
    let store = Store::from_data_root(&config.data_root);
    match action {
        KeyAction::Set { provider } => set(&store, &provider),
        KeyAction::List => list(&store),
        KeyAction::Remove { provider } => remove(&store, &provider),
    }
}

fn set(store: &Store, provider: &str) -> ExitCode {
    let mut key = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut key) {
        eprintln!("holosmith: key set: cannot read the key from stdin: {error}");
        return ExitCode::FAILURE;
    }
    // One trailing newline (and a shell's CR) is not part of the key. The store trims any other surrounding
    // whitespace, so a key pasted with a accidental blank line still works; the value is never echoed here.
    let key = key.strip_suffix('\n').unwrap_or(&key);
    let key = key.strip_suffix('\r').unwrap_or(key);
    match store.set_provider(provider, key) {
        Ok(()) => {
            println!("stored {provider}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("holosmith: key set: {error}");
            ExitCode::FAILURE
        }
    }
}

fn list(store: &Store) -> ExitCode {
    match store.providers() {
        Ok(providers) => {
            for provider in providers {
                println!("{provider}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("holosmith: key list: {error}");
            ExitCode::FAILURE
        }
    }
}

fn remove(store: &Store, provider: &str) -> ExitCode {
    match store.remove_provider(provider) {
        Ok(true) => {
            println!("removed {provider}");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!("holosmith: key remove: {provider} has no key");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("holosmith: key remove: {error}");
            ExitCode::FAILURE
        }
    }
}
