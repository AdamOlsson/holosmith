//! A tool call's full output, kept in a file rather than in the log (HS-033).
//!
//! Spec item 15 asks for "the full output of a tool call, inspectable"; the log is read by a phone, replayed
//! on every reconnect and folded by compaction, so a two-megabyte `cat` in a payload would make the transcript
//! heavy for ever. So a tool's output is written once, under the session's own directory, capped, and the
//! event carries a **preview** plus `output_ref` — the URL that fetches the rest
//! (`GET /api/sessions/{id}/tool-output/{tool_id}`, `http::tool_output`).
//!
//! **Why this is not in the mapping.** [`crate::rpc::Mapping`] is one record in, at most one frame out, with
//! no I/O at all — that is what lets a committed fixture be replayed without a data root, a daemon or a
//! database. The write therefore belongs to the turn's pump, which already owns the session id and the writer;
//! the mapping answers a [`crate::rpc::Mapped::Tool`] carrying the whole output and the pump hands it here.
//!
//! **The cap.** [`CAP`] is the spec's 5 MB, and it is a constant rather than a config key for the reason
//! HS-021's tick and ADR-0032's window are: it is a rule of the contract, not a property of a machine. The
//! body is truncated on a **char boundary**, so a file under `<tool_id>.txt` is always valid UTF-8, and one
//! marker line follows it naming how much was dropped — a reader must be able to tell a short answer from a
//! cut one without a header.
//!
//! **The id rule is an allow-list, not a shape.** pi's tool-call ids are the provider's (`call_<base32>`,
//! measured), so "32 hex characters" would refuse every real one. [`valid_id`] accepts a name and only a
//! name — 1 to [`MAX_ID`] characters of `[A-Za-z0-9_-]` — which is what makes a path traversal impossible by
//! construction instead of by sanitising: `.`, `/`, `\` and every control character are simply not in the
//! set, so `..` cannot be spelled. A refused id is a name no file can hold, and the answer everywhere is
//! "there is no such output" rather than a second reason an operator has to learn.
//!
//! **What the rule defends and what it does not.** The threat is a *name the agent chose*: a tool id arrives
//! from pi's records, so it is agent-controlled text, and joining it unchecked would be a path traversal. It is
//! not a defence against something that can already write inside `<data>/sessions/<id>/tool/` — a symlink planted
//! there and named `<tool_id>.txt` is followed by `std::fs::read`, and nothing in this build can create one: the
//! container's mounts are the worktree and the agent's own directory, not the data root's session directory.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::exit::{is_path_segment, session_dir};

/// The directory under a session's own directory that holds its tool outputs.
const TOOL: &str = "tool";

/// The file extension every stored output has.
const EXTENSION: &str = "txt";

/// The most output bytes a file holds.
///
/// The spec's number: five megabytes. The marker line that names a truncation is written **after** this many
/// bytes, so a file is at most `CAP` output bytes plus one line — the cap is on the output, which is what a
/// reader wants to know, and a marker that counted against it would be a lie about where the output stopped.
pub const CAP: usize = 5 * 1024 * 1024;

/// The most characters `tool.end`'s `output` carries.
///
/// The transcript's preview: enough for a stack trace's shape or a command's few lines, small enough that a
/// payload stays a payload. The rest is one `output_ref` away.
pub const PREVIEW_MAX: usize = 2000;

/// The most argument characters `tool.start` carries; a *summary* in the story's words.
pub const ARGUMENTS_MAX: usize = 2000;

/// The longest id [`valid_id`] accepts. A filename bound, not a second rule: pi's own ids are 30-odd
/// characters, and a name longer than this is one no provider mints.
pub const MAX_ID: usize = 128;

/// The line a cut file ends with: the bytes kept and the bytes there were.
///
/// A marker rather than a header, and appended to the body rather than written beside it, because the file
/// answers one question — "what did the tool print?" — and an operator who cats it must see that the answer
/// ends early. One function, so the writer and a test cannot spell it differently.
pub fn truncated_marker(kept: usize, total: usize) -> String {
    format!("[truncated: stored {kept} of {total} bytes]")
}

/// The ellipsis a preview or an argument summary ends with when it was cut. It is one character, so a
/// client that measures the string sees a difference between "all of it" and "the beginning".
pub const ELLIPSIS: char = '…';

/// One tool's output directory: `<data>/sessions/<id>/tool`.
///
/// A value rather than a free function so the pump holds one and a test builds one over a `TempDir`: the
/// root is the only thing either needs, and passing it explicitly is how a test proves nothing was written
/// outside it.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

/// What a stored output is, as the frame will spell it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The first [`PREVIEW_MAX`] characters, for `tool.end`'s `output`.
    pub preview: String,
    /// The path this endpoint serves it from, for `tool.end`'s `output_ref`.
    pub reference: String,
}

impl Store {
    /// The store over one machine's data root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The file one tool's output is written to, or `None` for an id that cannot be a filename.
    ///
    /// `None` rather than an error, because every caller asks the same question — "is there a file for
    /// this?" — and an id that cannot be one has the same answer as a missing file.
    pub fn path(&self, session_id: &str, tool_id: &str) -> Option<PathBuf> {
        if !valid_id(tool_id) || !is_path_segment(session_id) {
            return None;
        }
        Some(
            session_dir(&self.root, session_id)
                .join(TOOL)
                .join(format!("{tool_id}.{EXTENSION}")),
        )
    }

    /// Write one tool's output and answer what the frame should say about it.
    ///
    /// The whole output is written, capped at [`CAP`] bytes with the truncation marker after it. A write
    /// that fails is a `warn` and a `None`: the turn's frames are the transcript, and a full disk must not
    /// turn a session that did its work into an error. The caller then answers a frame whose `output` is the
    /// preview and whose `output_ref` is absent, which is the honest shape of "the text was printed and could
    /// not be kept".
    pub fn store(&self, session_id: &str, tool_id: &str, output: &str) -> Option<Stored> {
        let Some(path) = self.path(session_id, tool_id) else {
            tracing::warn!(
                target: "holosmith::tool",
                session = %session_id,
                tool = %tool_id,
                "a tool's output was not stored: its id is not a filename"
            );
            return None;
        };
        if let Err(error) = write(&path, output) {
            tracing::warn!(
                target: "holosmith::tool",
                session = %session_id,
                tool = %tool_id,
                path = %path.display(),
                %error,
                "a tool's output could not be written"
            );
            return None;
        }
        Some(Stored {
            preview: preview(output),
            reference: format!("/api/sessions/{session_id}/tool-output/{tool_id}"),
        })
    }

    /// Read one tool's output back, for the route that serves it.
    ///
    /// `None` for an id that cannot be a filename and for a file that is not there, which is one answer for
    /// one question: the caller's `404`.
    pub fn read(&self, session_id: &str, tool_id: &str) -> Option<Vec<u8>> {
        let path = self.path(session_id, tool_id)?;
        match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                // A file that exists and cannot be read is not "there is no such output": it is a machine
                // problem, and the operator needs it in the log rather than a 404 that hides it.
                tracing::error!(
                    target: "holosmith::tool",
                    session = %session_id,
                    tool = %tool_id,
                    path = %path.display(),
                    %error,
                    "a tool's output could not be read"
                );
                None
            }
        }
    }
}

/// The first [`PREVIEW_MAX`] characters of `text`, with an ellipsis when that is not all of it.
///
/// Characters rather than bytes: a preview is shown, so a multi-byte character cut in half would be a
/// replacement character on screen. The same rule the file's cap follows, for the same reason.
pub fn preview(text: &str) -> String {
    truncate(text, PREVIEW_MAX)
}

/// The first `max` characters of `text`, with [`ELLIPSIS`] when that is not all of it.
pub fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((cut, _)) => {
            let mut short = text[..cut].to_string();
            short.push(ELLIPSIS);
            short
        }
    }
}

/// Whether `tool_id` can be one filename's stem.
///
/// The rule is [`crate::exit::is_path_segment`]'s — no separator, no `.`, nothing a filesystem reads as more
/// than a name — plus a length bound, so a payload cannot become a filename of its own length.
pub fn valid_id(tool_id: &str) -> bool {
    is_path_segment(tool_id) && tool_id.len() <= MAX_ID
}

/// Write the file: the capped body, then the marker when it was cut.
///
/// Written once, with `create_new`'s sibling `truncate`, because a tool id is repeated only if the agent
/// repeats it — and a second write to one id should hold the second output rather than append to the first.
fn write(path: &Path, output: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(path)?;
    // The cut lands on a character boundary, so what is written is the longest valid prefix: the cap is a
    // byte count (that is what a file size is), and a preview is characters (that is what a screen shows).
    let cut = floor_boundary(output, CAP);
    let kept = &output[..cut];
    file.write_all(kept.as_bytes())?;
    if kept.len() != output.len() {
        // On its own line, so `tail -1` shows the marker whether or not the output ended with a newline.
        let marker = format!("\n{}\n", truncated_marker(kept.len(), output.len()));
        file.write_all(marker.as_bytes())?;
    }
    Ok(())
}

/// The largest index at or below `max` that is a character boundary of `text`.
///
/// A file is capped in bytes and cut where the text allows, so a multi-byte character is never split into two
/// halves of a broken one.
fn floor_boundary(text: &str, max: usize) -> usize {
    if text.len() <= max {
        return text.len();
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store over a fresh directory that is removed when the value drops.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "holosmith-tool-{}-{}-{name}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default()
            ));
            std::fs::create_dir_all(&path).expect("a scratch directory");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const SESSION: &str = "0123456789abcdef";
    const TOOL: &str = "call_00_9hlt8we6459oa420spsp3mju";

    #[test]
    fn an_id_is_a_name_and_only_a_name() {
        for good in [TOOL, "call_abc123", "a", "A-9_z"] {
            assert!(valid_id(good), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            "../app.db",
            "a/b",
            "a\\b",
            "a.b",
            "a b",
            "a\0b",
            "café",
        ] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        assert!(valid_id(&"a".repeat(MAX_ID)));
        assert!(!valid_id(&"a".repeat(MAX_ID + 1)));
    }

    #[test]
    fn an_output_under_the_cap_is_stored_whole_and_unmarked() {
        let scratch = Scratch::new("whole");
        let store = Store::new(&scratch.0);
        let text = "hello from the tool\n";
        let stored = store.store(SESSION, TOOL, text).expect("stored");
        assert_eq!(stored.preview, text);
        assert_eq!(
            stored.reference,
            format!("/api/sessions/{SESSION}/tool-output/{TOOL}")
        );
        let file = store.path(SESSION, TOOL).expect("a path");
        assert_eq!(std::fs::read(&file).expect("the file"), text.as_bytes());
        assert_eq!(
            store.read(SESSION, TOOL).expect("read back"),
            text.as_bytes()
        );
    }

    #[test]
    fn an_output_over_the_cap_is_cut_on_a_character_boundary_with_a_marker() {
        let scratch = Scratch::new("capped");
        let store = Store::new(&scratch.0);
        // Two bytes per character, so a byte-count cut would land inside one.
        let text = "é".repeat(CAP); // 2 × CAP bytes
        let stored = store.store(SESSION, TOOL, &text).expect("stored");
        assert_eq!(
            stored.preview.chars().count(),
            PREVIEW_MAX + 1,
            "the preview is the cap's worth of characters, however long the output was"
        );
        let file = store.path(SESSION, TOOL).expect("a path");
        let written = std::fs::read_to_string(&file).expect("the file is valid UTF-8");
        let (body, marker) = written.split_once('\n').expect("a body and a marker line");
        assert_eq!(body.len(), CAP, "the body is the cap, not a byte less");
        assert_eq!(body.chars().count(), CAP / 2);
        assert!(
            marker.contains(&CAP.to_string()) && marker.contains(&text.len().to_string()),
            "{marker}"
        );
        assert!(marker.starts_with("[truncated:"), "{marker}");
    }

    #[test]
    fn a_preview_is_short_and_a_short_output_is_untouched() {
        assert_eq!(preview("short"), "short");
        let long = "x".repeat(PREVIEW_MAX + 1);
        let short = preview(&long);
        assert_eq!(short.chars().count(), PREVIEW_MAX + 1);
        assert!(short.ends_with(ELLIPSIS));
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), format!("abc{ELLIPSIS}"));
    }

    #[test]
    fn an_id_that_cannot_be_a_filename_writes_nothing_and_reads_nothing() {
        let scratch = Scratch::new("refused");
        let store = Store::new(&scratch.0);
        assert!(store.store(SESSION, "../app.db", "secret").is_none());
        assert!(store.read(SESSION, "../app.db").is_none());
        assert!(
            !scratch.0.join("sessions").exists(),
            "a refused id creates no directory"
        );
    }

    #[test]
    fn a_session_that_is_not_a_path_segment_is_refused_rather_than_joined() {
        let scratch = Scratch::new("session");
        let store = Store::new(&scratch.0);
        assert!(store.path("..", TOOL).is_none());
        assert!(store.store("../../etc", TOOL, "x").is_none());
    }

    #[test]
    fn a_second_write_to_one_id_holds_the_second_output() {
        let scratch = Scratch::new("again");
        let store = Store::new(&scratch.0);
        store.store(SESSION, TOOL, "first").expect("stored");
        store.store(SESSION, TOOL, "second").expect("stored");
        assert_eq!(store.read(SESSION, TOOL).expect("read"), b"second");
    }
}
