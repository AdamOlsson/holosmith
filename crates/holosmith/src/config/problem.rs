//! One problem in the config file, with the position that makes it fixable.
//!
//! A line is `path:line:col: key: message`, or `path: key: message` when the problem is
//! not tied to a place in the file (a default that collides with a value the file sets,
//! for instance). Problems are printed in file order.

use std::fmt;
use std::path::{Path, PathBuf};

/// The 1-based line and column of the key that caused a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    /// Where a byte offset in `text` is.
    pub(crate) fn at_offset(text: &str, offset: usize) -> Self {
        let mut position = Self { line: 1, column: 1 };
        for (index, character) in text.char_indices() {
            if index >= offset {
                break;
            }
            if character == '\n' {
                position.line += 1;
                position.column = 1;
            } else {
                position.column += 1;
            }
        }
        position
    }
}

/// One thing wrong with one config file. The file path lives on [`Problems`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Problem {
    pub position: Option<Position>,
    pub key: Option<String>,
    pub message: String,
}

impl Problem {
    pub(crate) fn new(
        position: Option<Position>,
        key: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            position,
            key,
            message: message.into(),
        }
    }
}

/// Every problem found in one file, in file order, rendered with a count at the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problems {
    path: PathBuf,
    items: Vec<Problem>,
}

impl Problems {
    /// Sort by position, so the operator reads the file from the top down. A problem with
    /// no position has nowhere to sit in that order and comes last.
    pub(crate) fn new(path: &Path, mut items: Vec<Problem>) -> Self {
        items.sort_by_key(|problem| (problem.position.is_none(), problem.position));
        Self {
            path: path.to_path_buf(),
            items,
        }
    }
}

impl fmt::Display for Problems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for problem in &self.items {
            write!(f, "{}", self.path.display())?;
            if let Some(position) = problem.position {
                write!(f, ":{}:{}", position.line, position.column)?;
            }
            f.write_str(": ")?;
            if let Some(key) = &problem.key {
                write!(f, "{key}: ")?;
            }
            writeln!(f, "{}", problem.message)?;
        }
        let count = self.items.len();
        let noun = if count == 1 { "problem" } else { "problems" };
        writeln!(f, "{count} {noun} in {}", self.path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_lines_and_columns_from_one() {
        let text = "role = \"both\"\nmax_live_sessions = 0\n";
        assert_eq!(
            Position::at_offset(text, 0),
            Position { line: 1, column: 1 }
        );
        assert_eq!(
            Position::at_offset(text, 5),
            Position { line: 1, column: 6 }
        );
        assert_eq!(
            Position::at_offset(text, 14),
            Position { line: 2, column: 1 }
        );
    }

    #[test]
    fn the_count_agrees_with_the_number_of_lines() {
        let problems = Problems::new(
            Path::new("config.toml"),
            vec![
                Problem::new(None, Some("preview_listen".to_string()), "must differ"),
                Problem::new(
                    Some(Position { line: 3, column: 1 }),
                    Some("role".to_string()),
                    "unknown key",
                ),
            ],
        );
        assert_eq!(
            problems.to_string(),
            "config.toml:3:1: role: unknown key\nconfig.toml: preview_listen: must differ\n2 problems in config.toml\n"
        );
    }

    #[test]
    fn one_problem_is_reported_in_the_singular() {
        let problems = Problems::new(
            Path::new("config.toml"),
            vec![Problem::new(None, None, "a problem with no key")],
        );
        assert_eq!(
            problems.to_string(),
            "config.toml: a problem with no key\n1 problem in config.toml\n"
        );
    }
}
