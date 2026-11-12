//! `hosts.toml`, the seed every machine is given (spec item 16).
//!
//! Item 16's risk: "each machine reads an optional `hosts.toml` at startup and merges it into its hosts table (the
//! file wins on conflicts), so one file copied to every machine keeps the lists aligned". This is the reader. The
//! merge is [`super::merge`].
//!
//! The shape is the smallest one that holds a list, and it is written in TOML so an operator's editor knows what it
//! is:
//!
//! ```toml
//! # The machines this registry knows.
//! [[hosts]]
//! name = "workshop"
//! url = "https://workshop.tail1234.ts.net"
//!
//! [[hosts]]
//! name = "attic"
//! url = "https://attic.tail1234.ts.net:8443"
//! ```
//!
//! **Read by hand, strictly**, [`crate::vapid::parse`]'s taste at the size a list needs: any line that is not blank,
//! a `#` comment, a `[[hosts]]` header, or a `name`/`url` assignment inside a table is a refusal that names its line
//! number. The `toml` crate is not a dependency of this build and two keys do not earn it (ADR-0001) — but a file
//! somebody edited into something else must not be read as *some* of a registry, which is what leniency would do.
//!
//! **All-or-nothing.** One malformed line refuses the whole file, so the merge stores nothing and the boot says why.
//! A half-read seed would leave the table holding rows the operator never wrote in the order they happened to appear.
//!
//! The two checks a row still gets here are the ones that are about the *name*: its alphabet (it becomes a path
//! segment) and [`super::LOCAL`]'s reservation. The **host predicate and the port rule are the API's and
//! not this file's** (ADR-0073): `hosts.toml` is on the machine's own disk and is as trusted as `config.toml`, while
//! `POST /api/hosts` exists for values that arrive over a network.

use crate::config::Origin;
use crate::hosts;

/// The header that starts one host's table: item 16's file is a list, so it is an array of tables.
const HEADER: &str = "[[hosts]]";

/// One row the file names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub url: Origin,
}

/// Read `text`, or the sentence naming the line that is not a seed.
pub fn parse(text: &str) -> Result<Vec<Row>, String> {
    let mut rows: Vec<Row> = Vec::new();
    // The table being read: `Some` between a `[[hosts]]` and the next one.
    let mut open: Option<(usize, Option<String>, Option<String>)> = None;
    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == HEADER {
            if let Some((at, name, url)) = open.take() {
                rows.push(finish(at, name, url)?);
            }
            open = Some((line_number, None, None));
            continue;
        }
        let Some((at, name, url)) = open.as_mut() else {
            return Err(format!(
                "line {line_number}: {line:?} is outside a {HEADER} table, which is the only shape this file has"
            ));
        };
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "line {line_number}: {line:?} is not `key = \"value\"`"
            ));
        };
        let value = value
            .trim()
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or_else(|| format!("line {line_number}: {} is not a quoted string", key.trim()))?;
        match key.trim() {
            "name" if name.is_none() => *name = Some(value.to_string()),
            "url" if url.is_none() => *url = Some(value.to_string()),
            "name" | "url" => {
                return Err(format!(
                    "line {line_number}: `{}` is written twice in the table that starts on line {at}",
                    key.trim()
                ));
            }
            other => {
                return Err(format!(
                    "line {line_number}: `{other}` is not a key of a host; a table has `name` and `url`"
                ));
            }
        }
    }
    if let Some((at, name, url)) = open.take() {
        rows.push(finish(at, name, url)?);
    }
    Ok(rows)
}

/// One table's two values, once its header has been closed by the next one or the end of the file.
fn finish(at: usize, name: Option<String>, url: Option<String>) -> Result<Row, String> {
    let Some(name) = name else {
        return Err(format!("line {at}: the table has no `name`"));
    };
    let Some(url) = url else {
        return Err(format!("line {at}: the table for {name:?} has no `url`"));
    };
    if let Some(problem) = hosts::name_problem(&name) {
        return Err(format!("line {at}: {problem}"));
    }
    let url = Origin::parse(&url).map_err(|problem| format!("line {at}: `url` {problem}"))?;
    Ok(Row { name, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(text: &str) -> Origin {
        Origin::parse(text).expect("the test's origin parses")
    }

    #[test]
    fn two_tables_are_two_rows() {
        let rows = parse(
            r#"
# The machines this registry knows.
[[hosts]]
name = "workshop"
url = "https://workshop.tail1234.ts.net"

[[hosts]]
name = "attic"
url = "https://attic.tail1234.ts.net:8443"
"#,
        )
        .expect("a seed of two hosts");
        assert_eq!(
            rows,
            vec![
                Row {
                    name: "workshop".to_string(),
                    url: origin("https://workshop.tail1234.ts.net"),
                },
                Row {
                    name: "attic".to_string(),
                    url: origin("https://attic.tail1234.ts.net:8443"),
                },
            ]
        );
    }

    #[test]
    fn an_empty_file_is_an_empty_seed() {
        assert_eq!(parse("").expect("no rows"), Vec::new());
        assert_eq!(parse("# nothing yet\n\n").expect("no rows"), Vec::new());
    }

    #[test]
    fn any_line_the_shape_does_not_know_is_refused_with_its_number() {
        let cases: [(&str, &str); 8] = [
            // An assignment outside a table.
            ("name = \"workshop\"\n", "outside a"),
            // The single-table spelling, which is not a list.
            ("[hosts]\nname = \"workshop\"\n", "outside a"),
            // A key that is not a host's.
            (
                "[[hosts]]\nname = \"workshop\"\nnet = \"x\"\n",
                "not a key of a host",
            ),
            // A value that is not a quoted string.
            (
                "[[hosts]]\nname = workshop\nurl = \"https://a.b.ts.net\"\n",
                "quoted string",
            ),
            // A missing url, and a missing name.
            ("[[hosts]]\nname = \"workshop\"\n", "has no `url`"),
            ("[[hosts]]\nurl = \"https://a.b.ts.net\"\n", "has no `name`"),
            // A repeated key.
            (
                "[[hosts]]\nname = \"a\"\nname = \"b\"\nurl = \"https://a.b.ts.net\"\n",
                "written twice",
            ),
            // A line that is neither.
            (
                "[[hosts]]\nname = \"workshop\"\nurl = \"https://a.b.ts.net\"\n}",
                "not `key",
            ),
        ];
        for (text, needle) in cases {
            let error = parse(text).expect_err(text);
            assert!(error.contains(needle), "{text:?} answered {error:?}");
        }
        // A line number is part of the sentence: an operator has to find the line.
        let error = parse("[[hosts]]\nname = \"workshop\"\nnet = \"x\"\n").expect_err("a bad key");
        assert!(error.starts_with("line 3:"), "{error}");
    }

    #[test]
    fn the_name_rules_are_this_files_and_the_host_rule_is_not() {
        // `local` is reserved everywhere, and the alphabet is the same one the API applies.
        for (text, needle) in [
            (
                "[[hosts]]\nname = \"local\"\nurl = \"https://a.b.ts.net\"\n",
                "own sessions",
            ),
            (
                "[[hosts]]\nname = \"Work shop\"\nurl = \"https://a.b.ts.net\"\n",
                "lowercase",
            ),
        ] {
            let error = parse(text).expect_err(text);
            assert!(error.contains(needle), "{error}");
        }
        // A loopback URL is *not* refused here: the file is the operator's, and the host predicate is the API's.
        let rows = parse("[[hosts]]\nname = \"thisbox\"\nurl = \"http://127.0.0.1:8080\"\n")
            .expect("the file is not filtered by the host predicate");
        assert_eq!(rows.len(), 1);
        // A URL with a path is still not an origin, because the probe asks `GET /api/health` at it and nothing else.
        assert!(
            parse("[[hosts]]\nname = \"a\"\nurl = \"https://a.b.ts.net/api\"\n")
                .expect_err("a path is not an origin")
                .contains("path")
        );
    }
}
