//! Migration SQL read as text: split into statements, or read as code without its prose.
//!
//! Two callers need this and only one of them runs SQL: the runner splits a file so a failure
//! can name the statement that failed (ADR-0009), and the expand/contract rule reads the same
//! file as code, so a word in a comment or in a string literal cannot be mistaken for a
//! destructive statement.

/// Split a migration file into statements, so a failure can name the statement that failed.
///
/// The errors that matter in a migration carry no SQL text (only prepare-time errors do), so
/// the runner cannot lean on the error and splits the file itself. Each chunk then goes through
/// `Connection::execute`, which is a one-statement primitive: an under-split chunk is refused
/// as `MultipleStatement` and a trigger body cut in half fails loudly, never silently.
/// A chunk that holds only comments is dropped, because `execute` rejects one.
pub fn split_statements(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => index = skip_quoted(bytes, index),
            b'[' => index = skip_bracketed(bytes, index),
            b'-' if bytes.get(index + 1) == Some(&b'-') => index = skip_line(bytes, index),
            b'/' if bytes.get(index + 1) == Some(&b'*') => index = skip_block(bytes, index),
            b';' => {
                push_statement(&mut statements, &sql[start..index]);
                start = index + 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    push_statement(&mut statements, &sql[start..]);
    statements
}

/// The same file read as code: comments removed and the insides of string literals blanked, so
/// `-- renamed_at` or `'DROP TABLE events'` is not a destructive statement.
pub fn code_only(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut code = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => {
                index = skip_quoted(bytes, index);
                code.push(b' ');
            }
            b'[' => {
                index = skip_bracketed(bytes, index);
                code.push(b' ');
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index = skip_line(bytes, index);
                code.push(b'\n');
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block(bytes, index);
                code.push(b' ');
            }
            byte => {
                code.push(byte);
                index += 1;
            }
        }
    }
    // The input was UTF-8 and whole literals and comments are replaced, never split.
    String::from_utf8_lossy(&code).into_owned()
}

fn push_statement<'a>(statements: &mut Vec<&'a str>, chunk: &'a str) {
    let statement = strip_leading_comments(chunk);
    if has_code(statement) {
        statements.push(statement.trim());
    }
}

/// The chunk without the comment lines and blank lines before its first statement, so the
/// statement an error quotes is the SQL and not the file's header.
fn strip_leading_comments(chunk: &str) -> &str {
    let bytes = chunk.as_bytes();
    let mut index = 0;
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        match (bytes.get(index), bytes.get(index + 1)) {
            (Some(b'-'), Some(b'-')) => index = skip_line(bytes, index),
            (Some(b'/'), Some(b'*')) => index = skip_block(bytes, index),
            _ => break,
        }
    }
    &chunk[index..]
}

/// Whether a chunk holds anything but whitespace and comments.
fn has_code(chunk: &str) -> bool {
    let bytes = chunk.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'-' if bytes.get(index + 1) == Some(&b'-') => index = skip_line(bytes, index),
            b'/' if bytes.get(index + 1) == Some(&b'*') => index = skip_block(bytes, index),
            byte if byte.is_ascii_whitespace() => index += 1,
            _ => return true,
        }
    }
    false
}

/// Past the closing quote of a `'…'` or `"…"` literal, where a doubled quote is an escape.
fn skip_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

/// Past the closing `]` of a `[…]` identifier.
fn skip_bracketed(bytes: &[u8], start: usize) -> usize {
    match bytes[start..].iter().position(|byte| *byte == b']') {
        Some(offset) => start + offset + 1,
        None => bytes.len(),
    }
}

fn skip_line(bytes: &[u8], start: usize) -> usize {
    match bytes[start..].iter().position(|byte| *byte == b'\n') {
        Some(offset) => start + offset + 1,
        None => bytes.len(),
    }
}

fn skip_block(bytes: &[u8], start: usize) -> usize {
    let body = &bytes[start + 2..];
    match body.windows(2).position(|pair| pair == b"*/") {
        Some(offset) => start + 2 + offset + 2,
        None => bytes.len(),
    }
}

/// The guard that keeps a table's SQL in the file that owns it, shared by every repository that
/// has one.
///
/// Two copies of this matcher existed — one in each repository — and had already drifted. Worse,
/// both named their scratch directory `holosmith-guard-{label}-{pid}` with the same labels, and
/// two guards in one crate share the lib-test binary and so the process id: the events guard's
/// `remove_dir_all` deleted the sessions guard's live fixture mid-run. One copy cannot collide
/// with itself, because the table word is part of every directory name it makes (S1).
#[cfg(test)]
pub(crate) mod guard {
    use std::path::{Path, PathBuf};

    /// Every file under `root`, relative to it with `/` separators, and its text.
    ///
    /// The walk several source-level rules share: a table's SQL location, and the rule that
    /// only one function writes a session's state. One copy, because the second caller would
    /// otherwise reimplement the recursion and quietly disagree about, say, `.rs` suffixes.
    pub(crate) fn sources_under(root: &Path) -> Vec<(String, String)> {
        let mut sources = Vec::new();
        let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if file_type.is_dir() {
                    pending.push(path);
                } else if file_type.is_file() {
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(relative) = path.strip_prefix(root) else {
                        continue;
                    };
                    sources.push((relative.to_string_lossy().replace('\\', "/"), text));
                }
            }
        }
        sources
    }

    /// One table's guard: its word, the spellings that name it, and the file allowed to hold
    /// them.
    pub(crate) struct Guard {
        /// The bare table name, e.g. `events`. It is also the label of every scratch directory
        /// this guard makes, which is what keeps two guards in one test binary apart.
        table: &'static str,
        /// The one file allowed to name the table in a statement, relative to `src/`.
        repository: &'static str,
        /// The spellings a statement naming the table can take, longest first so a
        /// `delete from …` is reported as itself rather than as `from …`.
        statements: &'static [&'static str],
    }

    impl Guard {
        pub(crate) fn new(
            table: &'static str,
            repository: &'static str,
            statements: &'static [&'static str],
        ) -> Self {
            Self {
                table,
                repository,
                statements,
            }
        }

        /// Every statement naming the table in a file under `root` that is not the repository's,
        /// as `<file>:<line>: <phrase>`.
        pub(crate) fn offenders_under(&self, root: &Path) -> Vec<String> {
            let mut offenders = Vec::new();
            for (file, text) in sources_under(root) {
                if file == self.repository {
                    continue;
                }
                let (normalised, lines) = self.normalise(&text);
                if let Some((at, statement)) = self.statement(&normalised) {
                    let line = lines[normalised[..at].chars().count()];
                    offenders.push(format!("{file}:{line}: {statement}"));
                }
            }
            offenders
        }

        /// A directory of planted files that removes itself, so a test leaves nothing in `/tmp`.
        ///
        /// `purpose` names the test within the table (`planted`, `spelling`); the table word
        /// separates the guards sharing this binary.
        pub(crate) fn temp_dir(&self, purpose: &str) -> TempDir {
            TempDir::new(&format!("{}-{purpose}", self.table))
        }

        /// The first statement normalised `text` holds, as its offset and the phrase that
        /// matched. The phrases are tried in order, so a longer statement is reported as itself
        /// rather than as the shorter one it also contains.
        fn statement(&self, normalised: &str) -> Option<(usize, &'static str)> {
            self.statements
                .iter()
                .find_map(|statement| names_word(normalised, statement).map(|at| (at, *statement)))
        }

        /// A file's text as the guard matches it, and the line of the original each character
        /// came from, so a statement split across lines is still reported where it was written.
        ///
        /// The per-file normalisation is what makes a spelling like `FROM main.<table>` or a
        /// statement broken by a Rust `\`-continuation visible: the patterns are literal, so the
        /// text is rewritten into the one spelling `statements` holds before they are applied.
        fn normalise(&self, text: &str) -> (String, Vec<usize>) {
            let (mut normalised, mut lines) = squashed(&text.to_ascii_lowercase());
            // `FROM main.<table>` is `FROM <table>`: `main` is the schema SQLite attaches by
            // default, and it is not part of the statement the guard names.
            rewrite(
                &mut normalised,
                &mut lines,
                &format!("main.{}", self.table),
                self.table,
            );
            // `INSERT OR REPLACE`/`UPDATE OR IGNORE`: the conflict clause sits between the
            // keyword and the table, and `statements` does not name it.
            for clause in ["or replace ", "or ignore "] {
                for keyword in ["insert ", "update "] {
                    rewrite(
                        &mut normalised,
                        &mut lines,
                        &format!("{keyword}{clause}"),
                        keyword,
                    );
                }
            }
            (normalised, lines)
        }
    }

    /// Where `needle` appears in normalised `haystack` with neither end inside a longer word: the
    /// table word is a statement, and a longer identifier that merely starts with it is not.
    fn names_word(haystack: &str, needle: &str) -> Option<usize> {
        let mut from = 0;
        while let Some(index) = haystack[from..].find(needle) {
            let start = from + index;
            let end = start + needle.len();
            let before = haystack[..start].chars().next_back();
            let after = haystack[end..].chars().next();
            if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
                return Some(start);
            }
            from = start + 1;
        }
        None
    }

    /// `text` with `\`-continuations joined, every run of whitespace collapsed to one space, and
    /// the quoting a word can wear (`"events"`, `` `events` ``, `[events]`, and the backslash
    /// Rust needs to escape a quote inside one) removed.
    ///
    /// `lines[i]` is the line of `text` that character `i` came from, so a statement found in the
    /// squashed text is still reported on the line its first keyword is written on.
    fn squashed(text: &str) -> (String, Vec<usize>) {
        let mut squashed = String::with_capacity(text.len());
        let mut lines = Vec::with_capacity(text.len());
        let mut line = 1;
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            // A `\`-continuation is not a break at all: Rust drops the backslash, the newline and
            // the next line's indentation, so the statement's two source lines are one statement
            // here too.
            if character == '\\' && characters.peek() == Some(&'\n') {
                characters.next();
                line += 1;
                while characters
                    .peek()
                    .is_some_and(|next| next.is_whitespace() && *next != '\n')
                {
                    characters.next();
                }
                continue;
            }
            if character == '\n' {
                line += 1;
            }
            if character.is_whitespace() {
                if !squashed.is_empty() && !squashed.ends_with(' ') {
                    squashed.push(' ');
                    lines.push(line);
                }
                continue;
            }
            if matches!(character, '"' | '`' | '[' | ']' | '\\') {
                continue;
            }
            squashed.push(character);
            lines.push(line);
        }
        (squashed, lines)
    }

    /// Replace every `needle` in `normalised` with `replacement`, keeping `lines` — one entry
    /// per character of `normalised` — aligned: a rewritten character keeps the line of the
    /// spelling it replaced.
    fn rewrite(normalised: &mut String, lines: &mut Vec<usize>, needle: &str, replacement: &str) {
        let mut from = 0;
        while let Some(index) = normalised[from..].find(needle) {
            let start = from + index;
            let start_at = normalised[..start].chars().count();
            let line = lines[start_at];
            let needle_chars = needle.chars().count();
            normalised.replace_range(start..start + needle.len(), replacement);
            lines.splice(
                start_at..start_at + needle_chars,
                std::iter::repeat_n(line, replacement.chars().count()),
            );
            from = start + replacement.len();
        }
    }

    fn is_word(character: char) -> bool {
        character.is_ascii_alphanumeric() || character == '_'
    }

    /// A directory of planted files that removes itself, so the tests leave nothing in `/tmp`.
    pub(crate) struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        /// A directory the label names, removing anything a previous run left there.
        ///
        /// The label is the whole name: `Guard::temp_dir` prefixes its table word, so two guards
        /// sharing this test binary cannot collide, and a rule that has no table names its own.
        pub(crate) fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("holosmith-guard-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("the planted directory is creatable");
            Self { path }
        }

        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        /// Write `text` to `file`, creating the directories a nested path names — a rule about a
        /// file like `sessions/state.rs` needs its fixture at that path, not at its basename.
        pub(crate) fn plant(&self, file: &str, text: &str) {
            let path = self.path.join(file);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the planted directories are creatable");
            }
            std::fs::write(path, text).expect("the planted file is writable");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statements_of_a_file_are_split_on_semicolons() {
        assert_eq!(
            split_statements("CREATE TABLE a (id INTEGER);\nCREATE INDEX b ON a (id);\n"),
            vec!["CREATE TABLE a (id INTEGER)", "CREATE INDEX b ON a (id)"]
        );
        assert_eq!(split_statements("SELECT 1"), vec!["SELECT 1"]);
        assert!(split_statements("").is_empty());
    }

    #[test]
    fn a_comment_only_chunk_is_dropped() {
        let sql = "-- the header\n-- names the story\nCREATE TABLE a (id INTEGER);\n\
                   /* trailing prose */\n";
        assert_eq!(split_statements(sql), vec!["CREATE TABLE a (id INTEGER)"]);
        assert!(split_statements("-- only a comment\n").is_empty());
    }

    #[test]
    fn a_semicolon_inside_a_literal_or_a_comment_does_not_split() {
        let sql = "INSERT INTO a VALUES ('one; two');\nINSERT INTO a VALUES (\"x;y\");\n\
                   INSERT INTO a VALUES (1); -- a trailing; comment\n";
        assert_eq!(
            split_statements(sql),
            vec![
                "INSERT INTO a VALUES ('one; two')",
                "INSERT INTO a VALUES (\"x;y\")",
                "INSERT INTO a VALUES (1)",
            ],
            "the comment after the last semicolon is not a statement of its own"
        );
    }

    #[test]
    fn a_bracketed_identifier_and_a_doubled_quote_do_not_split() {
        assert_eq!(
            split_statements("CREATE TABLE [a;b] (id INTEGER);"),
            vec!["CREATE TABLE [a;b] (id INTEGER)"]
        );
        assert_eq!(
            split_statements("INSERT INTO a VALUES ('it''s; here');"),
            vec!["INSERT INTO a VALUES ('it''s; here')"]
        );
    }

    #[test]
    fn an_unterminated_quote_or_comment_takes_the_rest_of_the_file() {
        assert_eq!(
            split_statements("INSERT INTO a VALUES ('open; ended'"),
            vec!["INSERT INTO a VALUES ('open; ended'"]
        );
        assert!(split_statements("/* never closed; really").is_empty());
        assert_eq!(
            split_statements("-- no newline at the end"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn code_only_drops_comments_and_the_insides_of_literals() {
        assert_eq!(
            code_only("ALTER TABLE a ADD COLUMN b INTEGER; -- a note\n/* another */"),
            "ALTER TABLE a ADD COLUMN b INTEGER; \n "
        );
        assert_eq!(
            code_only("INSERT INTO a VALUES ('DROP TABLE a');"),
            "INSERT INTO a VALUES ( );"
        );
        assert_eq!(
            code_only("CREATE TABLE [drop table] (id INTEGER);"),
            "CREATE TABLE   (id INTEGER);"
        );
    }
}
