//! `holosmith key set|list|remove|revoke` (spec CLI phase 1).
//!
//! `set` reads the key from **stdin only**. A key in `argv` is a key in the process list, in the shell's
//! history and in any crash report; the spec says stdin, so there is no `--key` flag to get it wrong with.
//! `list` prints ids and never values.
//!
//! **One file, two tables, and one command per table's half of a rotation.** `set` takes an id from either table
//! and stores it where it belongs; `remove` is a model provider's other half of a rotation and `revoke` a tool's,
//! so a wrong-table id is refused with the command that will work instead (ADR-0098). `revoke` is idempotent: a
//! tool with no key is the state the operator asked for, so it prints one sentence and exits 0, while `remove`
//! still reports a provider with no key as a failure because its sentence is the only evidence a rotation ran.
//!
//! **Stdin has two shapes, and a terminal is the one that needs saying out loud.** A pipe — every test, and
//! `printf '…' | holosmith key set <id>` — is read to EOF with no prompt, because a prompt there would land in
//! whatever captured stderr. A terminal gets one sentence on stderr, the terminal's echo off, and a single
//! **Enter**-terminated line. Reading to EOF on a terminal is what the command did before, and it is
//! indistinguishable from a hang: the screen stays blank, typing shows nothing unusual, and only a Ctrl-D (or a
//! Ctrl-C, which loses the key) ends it. ADR-0095 records the decision.

use std::io::{BufRead, IsTerminal, Read};
use std::process::{Command, ExitCode, Stdio};

use crate::cli::KeyAction;
use crate::config::{Config, LoadError};
use crate::key::{Store, refusal_for_remove, refusal_for_revoke, unknown_id};

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
        KeyAction::Set { id } => set(&store, &id),
        KeyAction::List => list(&store),
        KeyAction::Remove { provider } => remove(&store, &provider),
        KeyAction::Revoke { tool } => revoke(&store, &tool),
    }
}

fn set(store: &Store, id: &str) -> ExitCode {
    // Before stdin, not after: an id from neither table would otherwise sit waiting for a key that the store then
    // refuses anyway, which is the same blank screen as the missing prompt.
    let into_tools = crate::key::tool(id).is_some();
    if !into_tools && crate::key::provider(id).is_none() {
        eprintln!("holosmith: key set: {}", unknown_id(id));
        return ExitCode::FAILURE;
    }
    let key = match read_key(id) {
        Ok(key) => key,
        Err(error) => {
            eprintln!("holosmith: key set: {error}");
            return ExitCode::FAILURE;
        }
    };
    let stored = if into_tools {
        store.set_tool(id, &key)
    } else {
        store.set_provider(id, &key)
    };
    match stored {
        Ok(()) => {
            println!("stored {id}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("holosmith: key set: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The key, from a pipe or from a terminal, without the line ending the terminal's Enter adds.
fn read_key(id: &str) -> Result<String, String> {
    let stdin = std::io::stdin();
    let mut key = String::new();
    if stdin.is_terminal() {
        // The guard is dropped — echo restored — before `stored` is printed, and on the read's own error path;
        // a Ctrl-C during the read kills this process with no code run, and the operator's shell puts its own
        // modes back when it prompts again.
        let echo = Echo::off();
        eprintln!("{}", prompt(id, echo.hidden));
        if let Err(error) = stdin.lock().read_line(&mut key) {
            return Err(format!("cannot read the key from stdin: {error}"));
        }
    } else if let Err(error) = stdin.lock().read_to_string(&mut key) {
        return Err(format!("cannot read the key from stdin: {error}"));
    }
    // One trailing newline (and a shell's CR) is not part of the key. The store trims any other surrounding
    // whitespace, so a key pasted with an accidental blank line still works; the value is never echoed here.
    let key = key.strip_suffix('\n').unwrap_or(&key);
    Ok(key.strip_suffix('\r').unwrap_or(key).to_string())
}

/// What the operator is told before the key, which has to name how the line ends **and** whether it will show.
///
/// A hidden field with no warning is the original bug in a new costume: nothing appears as the operator types,
/// so a prompt that stayed silent about it would look like the hang it replaced.
fn prompt(id: &str, hidden: bool) -> String {
    if hidden {
        format!("paste the {id} API key, then press Enter (nothing is shown as you type)")
    } else {
        format!(
            "paste the {id} API key, then press Enter (the key will be visible as you type: `stty` is not on PATH)"
        )
    }
}

/// The terminal's echo, off for the length of one read, through one `stty` child.
///
/// There is no `termios` here to call: this is safe Rust in a static binary, and `stty` is the same shape as
/// `server deploy`'s file actions, one child per small job. A host without `stty` keeps the prompt and loses
/// only the hiding, and the prompt says so rather than claiming a field it cannot hide.
struct Echo {
    hidden: bool,
}

impl Echo {
    fn off() -> Self {
        let hidden = Command::new("stty")
            .arg("-echo")
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        Self { hidden }
    }
}

impl Drop for Echo {
    fn drop(&mut self) {
        if !self.hidden {
            return;
        }
        let restored = Command::new("stty")
            .arg("echo")
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !restored {
            eprintln!(
                "holosmith: key set: this terminal's echo could not be restored: run `stty echo`"
            );
        }
    }
}

fn list(store: &Store) -> ExitCode {
    let providers = match store.providers() {
        Ok(providers) => providers,
        Err(error) => {
            eprintln!("holosmith: key list: {error}");
            return ExitCode::FAILURE;
        }
    };
    let tools = match store.tools() {
        Ok(tools) => tools,
        Err(error) => {
            eprintln!("holosmith: key list: {error}");
            return ExitCode::FAILURE;
        }
    };
    // A provider id is printed alone, which is the line this command has always printed and the line a script
    // parses; a tool id carries the word that says which table its key is in, because the two are not the same
    // kind of credential and only one of them has a proxy in this build.
    for provider in providers {
        println!("{provider}");
    }
    for tool in tools {
        println!("{tool} (tool)");
    }
    ExitCode::SUCCESS
}

fn remove(store: &Store, provider: &str) -> ExitCode {
    // A tool's key is `key revoke`'s, so the id is refused with that command's name rather than reported as a
    // provider with no key — which is what this command was never going to find.
    if let Some(refusal) = refusal_for_remove(provider) {
        eprintln!("holosmith: key remove: {refusal}");
        return ExitCode::FAILURE;
    }
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

fn revoke(store: &Store, tool: &str) -> ExitCode {
    if let Some(refusal) = refusal_for_revoke(tool) {
        eprintln!("holosmith: key revoke: {refusal}");
        return ExitCode::FAILURE;
    }
    if crate::key::tool(tool).is_none() {
        eprintln!("holosmith: key revoke: {}", unknown_id(tool));
        return ExitCode::FAILURE;
    }
    match store.remove_tool(tool) {
        Ok(true) => {
            println!("revoked {tool}");
            ExitCode::SUCCESS
        }
        // Not an error: the operator asked for a tool with no stored key, and that is the state it is in.
        Ok(false) => {
            println!("no {tool} key to revoke");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("holosmith: key revoke: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::prompt;

    /// The sentence that replaced a blank screen names the provider, the key that ends the line and whether the
    /// characters will show: all three are what the operator cannot see for themselves.
    #[test]
    fn the_prompt_names_the_provider_the_key_and_whether_the_input_is_hidden() {
        let hidden = prompt("opencode-go", true);
        assert!(hidden.contains("opencode-go"), "{hidden}");
        assert!(hidden.contains("press Enter"), "{hidden}");
        assert!(hidden.contains("nothing is shown"), "{hidden}");
        assert!(!hidden.contains("visible"), "{hidden}");

        let echoed = prompt("anthropic", false);
        assert!(echoed.contains("anthropic"), "{echoed}");
        assert!(echoed.contains("press Enter"), "{echoed}");
        assert!(
            echoed.contains("will be visible"),
            "a prompt that cannot hide the input says so: {echoed}"
        );
        assert!(
            !echoed.contains("nothing is shown"),
            "a prompt that hides nothing must not claim to: {echoed}"
        );
    }
}
