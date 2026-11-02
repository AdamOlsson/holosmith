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
