//! `config edit` runs the operator's own editor: `$VISUAL`, then `$EDITOR`, nothing else.
//!
//! There is no `vi` fallback: an editor we picked would open on a machine with no tty and
//! look like a hang. The command line is split with `shlex`, so `code --wait` works.

use std::env;
use std::fmt;
use std::io;
use std::path::Path;
use std::process::Command;

/// Why no editor could be run.
#[derive(Debug)]
pub enum EditorError {
    /// Neither variable is set, or the one that is set is empty.
    NotSet,
    /// The variable is set but cannot be split into a command line.
    Unparsable {
        variable: &'static str,
        value: String,
    },
    /// The editor could not be started.
    Spawn { program: String, source: io::Error },
}

impl fmt::Display for EditorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotSet => f.write_str("neither $VISUAL nor $EDITOR is set"),
            Self::Unparsable { variable, value } => {
                write!(f, "${variable} is not a shell-splittable command: {value}")
            }
            Self::Spawn { program, source } => write!(f, "cannot run {program}: {source}"),
        }
    }
}

/// The editor from the environment, ready to run on a file.
#[derive(Debug)]
pub struct Editor {
    program: String,
    arguments: Vec<String>,
}

impl Editor {
    /// `$VISUAL` wins over `$EDITOR` (the git and POSIX convention). An empty value is the
    /// same as unset.
    pub fn from_env() -> Result<Self, EditorError> {
        let (variable, value) = [
            ("VISUAL", env::var("VISUAL")),
            ("EDITOR", env::var("EDITOR")),
        ]
        .into_iter()
        .filter_map(|(variable, value)| value.ok().map(|value| (variable, value)))
        .find(|(_, value)| !value.trim().is_empty())
        .ok_or(EditorError::NotSet)?;
        // A value that does not split into at least a program cannot start an editor.
        let Some((program, arguments)) = shlex::split(&value).and_then(into_command) else {
            return Err(EditorError::Unparsable { variable, value });
        };
        Ok(Self { program, arguments })
    }

    /// Run the editor on `path` with our own stdio, and report its exit code. A signalled
    /// editor has no exit code; it is reported as a failure.
    pub fn run(&self, path: &Path) -> Result<i32, EditorError> {
        let status = Command::new(&self.program)
            .args(&self.arguments)
            .arg(path)
            .status()
            .map_err(|source| EditorError::Spawn {
                program: self.program.clone(),
                source,
            })?;
        Ok(status.code().unwrap_or(1))
    }
}

/// The program and its arguments, or nothing when there are no words at all.
fn into_command(mut words: Vec<String>) -> Option<(String, Vec<String>)> {
    if words.is_empty() {
        return None;
    }
    let program = words.remove(0);
    Some((program, words))
}
