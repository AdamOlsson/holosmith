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
//!
//! **An image in a result is a second file under the same rule** (HS-035): `<tool_id>-<block index>.<ext>`,
//! where the extension comes from pi's `mimeType` and a type this build cannot name is kept anyway under
//! `.bin`. The bytes are base64 in the record and decoded here (`decode_base64`) rather than in the mapping,
//! the bound is checked from the encoded length *before* anything is allocated, and the frame carries
//! `images: [{index, mime, output_ref}]` rather than a byte of the picture — for the same reason a tool's output
//! is a file: the log is replayed to a phone on every reconnect and folded by compaction (ADR-0032). The
//! extension is the *store's* knowledge, so `GET …/tool-image/{tool_id}/{index}` never takes a type from the
//! request.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::exit::{is_path_segment, session_dir};

/// The directory under a session's own directory that holds its tool outputs.
const TOOL: &str = "tool";

/// The file extension every stored output has.
const EXTENSION: &str = "txt";

/// The most bytes one stored image holds.
///
/// The spec's five megabytes, for the reason [`CAP`] is a constant rather than a config key: it is a rule of
/// the contract, not a property of a machine. A *decoded* byte count, because that is what a file's size is
/// and what a browser has to fetch; the base64 a record carries is a third larger and is not the thing being
/// bounded.
pub const IMAGE_CAP: usize = 5 * 1024 * 1024;

/// The image types this build can name, extension first and its content type second.
///
/// Four, because they are the four a browser draws and this build keeps no image library: naming a type it
/// cannot recognise would be a guess about bytes it never looked at.
const IMAGE_TYPES: [(&str, &str); 4] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// The extension an image of a type this build cannot name is stored under.
const UNNAMED_EXTENSION: &str = "bin";

/// What a file under [`UNNAMED_EXTENSION`] is served as.
///
/// `application/octet-stream` rather than a guessed image type: the bytes are kept, the *frame* carries the
/// `mime` pi reported, and a machine that cannot name a type must not claim one.
const UNNAMED_TYPE: &str = "application/octet-stream";

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

/// One image block of a tool's result, before anything is stored (HS-035).
///
/// pi's block is `{"type": "image", "mimeType": "image/png", "data": "<base64>"}` inside
/// `tool_execution_end.result.content`. The mapping answers one of these and the pump stores it, for the same
/// reason [`crate::rpc::Mapped::Tool`] carries the whole output: the mapping does no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The block's index in `result.content`, which is what the reference names.
    ///
    /// The block's own position rather than a counter of images, so a frame's entry says where in the result
    /// it came from and two ends of one call cannot disagree about which is which.
    pub index: usize,
    /// pi's `mimeType`: `image/png`, `image/jpeg`, `image/gif` or `image/webp` in practice.
    pub mime: String,
    /// pi's `data`: the image, base64.
    pub data: String,
}

/// One stored image, as the frame spells it.
///
/// No `preview` and no `bytes`: an image is drawn from its reference, and a payload that carried even a
/// thumbnail would be the log (replayed on every reconnect, folded by compaction) carrying image data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImage {
    /// The block's index, echoed from [`Image::index`].
    pub index: usize,
    /// pi's `mimeType`, which is the authority for a client: the file's extension says the same thing when
    /// this build could name it, and nothing when it could not.
    pub mime: String,
    /// The path this endpoint serves it from, for `tool.end`'s `images[].output_ref`.
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

    /// The directory a tool's files are in, or `None` for a session or an id that cannot be a path segment.
    ///
    /// The one place a session's tool directory is spelled, so the text file, the images and the read side
    /// cannot disagree about where a tool's files live.
    fn directory(&self, session_id: &str, tool_id: &str) -> Option<PathBuf> {
        if !valid_id(tool_id) || !is_path_segment(session_id) {
            return None;
        }
        Some(session_dir(&self.root, session_id).join(TOOL))
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

    /// Store one tool's images and answer what the frame should say about the ones that were written.
    ///
    /// Each image is decoded and written beside the text file, under a name built from the tool id and the
    /// block's index. A refused image — an invalid base64 body, one over [`IMAGE_CAP`], an id that cannot be a
    /// filename or a full disk — is a `warn` and no entry, so the frame's `images` names exactly the files that
    /// exist and a dead reference cannot be produced.
    ///
    /// **A second result for one tool id replaces the first**, the rule [`Store::store`] keeps for the text,
    /// and it includes the *type*: the stale names are removed before the new one is written, so a tool that
    /// answered a PNG once and a GIF the next time cannot leave a reader looking at the older image under the
    /// extension the read side tries first.
    pub fn store_images(
        &self,
        session_id: &str,
        tool_id: &str,
        images: &[Image],
    ) -> Vec<StoredImage> {
        let Some(directory) = self.directory(session_id, tool_id) else {
            tracing::warn!(
                target: "holosmith::tool",
                session = %session_id,
                tool = %tool_id,
                "a tool's images were not stored: its id is not a filename"
            );
            return Vec::new();
        };
        let mut stored = Vec::with_capacity(images.len());
        for image in images {
            // The bound is checked from the encoded length, before a byte is allocated for the decoded body:
            // a record that claims a gigabyte of image must not be a gigabyte of memory first.
            let Some(bytes) = decode_base64(&image.data) else {
                tracing::warn!(
                    target: "holosmith::tool",
                    session = %session_id,
                    tool = %tool_id,
                    index = image.index,
                    mime = %image.mime,
                    "an image in a tool result was not stored: its data is not base64 under the cap"
                );
                continue;
            };
            let extension = extension_of(&image.mime);
            let path = directory.join(format!("{}.{extension}", stem(tool_id, image.index)));
            remove_siblings(&directory, tool_id, image.index);
            if let Err(error) = write_bytes(&path, &bytes) {
                tracing::warn!(
                    target: "holosmith::tool",
                    session = %session_id,
                    tool = %tool_id,
                    index = image.index,
                    path = %path.display(),
                    %error,
                    "an image in a tool result could not be written"
                );
                continue;
            }
            stored.push(StoredImage {
                index: image.index,
                mime: image.mime.clone(),
                reference: image_reference(session_id, tool_id, image.index),
            });
        }
        stored
    }

    /// The file one stored image is in, with the content type its extension gives it.
    ///
    /// `None` for an id that cannot be a filename and for an image that is not there. The extension is not in
    /// the request: it is the *store's* knowledge, so a client can name an image by the pair the frame gave it
    /// and the type cannot be talked into something else by the request.
    pub fn image(
        &self,
        session_id: &str,
        tool_id: &str,
        index: usize,
    ) -> Option<(PathBuf, &'static str)> {
        let directory = self.directory(session_id, tool_id)?;
        for (extension, mime) in IMAGE_TYPES
            .iter()
            .copied()
            .chain(std::iter::once((UNNAMED_EXTENSION, UNNAMED_TYPE)))
        {
            let path = directory.join(format!("{}.{extension}", stem(tool_id, index)));
            if std::fs::metadata(&path).is_ok() {
                return Some((path, mime));
            }
        }
        None
    }

    /// Read one stored image back, with the content type to serve it as.
    ///
    /// `None` for an id that cannot be a filename and for a file that is not there — one answer for one
    /// question, the caller's `404`.
    pub fn read_image(
        &self,
        session_id: &str,
        tool_id: &str,
        index: usize,
    ) -> Option<(Vec<u8>, &'static str)> {
        let (path, mime) = self.image(session_id, tool_id, index)?;
        match std::fs::read(&path) {
            Ok(bytes) => Some((bytes, mime)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::error!(
                    target: "holosmith::tool",
                    session = %session_id,
                    tool = %tool_id,
                    path = %path.display(),
                    %error,
                    "a stored image could not be read"
                );
                None
            }
        }
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

/// One stored image's filename stem: the tool's id and the block's index.
///
/// A `-` between them and never a `.`, because the extension is the only dot these files have — the read side
/// tries [`IMAGE_TYPES`]' extensions in order, so a second dot would be a second thing to parse.
fn stem(tool_id: &str, index: usize) -> String {
    format!("{tool_id}-{index}")
}

/// The extension an image's content type is stored under.
///
/// A type this build cannot name is stored anyway, under [`UNNAMED_EXTENSION`]: a picture an operator's tool
/// produced is worth keeping even when this build cannot say what it is, and the frame still carries pi's own
/// `mime`.
fn extension_of(mime: &str) -> &'static str {
    IMAGE_TYPES
        .iter()
        .find(|(_, named)| *named == mime)
        .map(|(extension, _)| *extension)
        .unwrap_or(UNNAMED_EXTENSION)
}

/// The reference a frame names one stored image by.
fn image_reference(session_id: &str, tool_id: &str, index: usize) -> String {
    format!("/api/sessions/{session_id}/tool-image/{tool_id}/{index}")
}

/// Remove every name one image could have been stored under, so a second result replaces the first.
///
/// A tool id repeated with a different type is the case: without this, the earlier `.png` would still answer a
/// request for the later `.bin`, and the read side would hand back an image the frame no longer names.
fn remove_siblings(directory: &Path, tool_id: &str, index: usize) {
    for (extension, _) in IMAGE_TYPES
        .iter()
        .copied()
        .chain(std::iter::once((UNNAMED_EXTENSION, UNNAMED_TYPE)))
    {
        let path = directory.join(format!("{}.{extension}", stem(tool_id, index)));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::debug!(
                target: "holosmith::tool",
                path = %path.display(),
                %error,
                "a superseded image could not be removed"
            ),
        }
    }
}

/// Write bytes: the file, and the directory it lives in when it is not there yet.
fn write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// The bytes of a base64 body, or `None` when it is not base64 or is larger than [`IMAGE_CAP`].
///
/// Written out rather than pulled in: the one input is pi's `data` field, the alphabet is the standard one, and
/// a dependency for forty lines is a dependency this build would then have to keep current. **The cap is
/// checked from the encoded length first**, before anything is allocated, because the input is
/// agent-controlled text: a record claiming a gigabyte of image must not be a gigabyte of memory on the way to
/// being refused. Padding is optional — pi writes it, and a producer that left it off still has a decodable
/// body — but a `=` anywhere except the end is not base64.
pub fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let raw = text.as_bytes();
    let mut end = raw.len();
    let mut padding = 0;
    while end > 0 && raw[end - 1] == b'=' && padding < 2 {
        end -= 1;
        padding += 1;
    }
    if end == 0 && !raw.is_empty() {
        return None;
    }
    if raw[..end].contains(&b'=') {
        return None;
    }
    if padding > 0 && !raw.len().is_multiple_of(4) {
        return None;
    }
    // A group of one is not a group: `A` alone decodes to nothing and is what a truncated body looks like.
    if end % 4 == 1 {
        return None;
    }
    if end / 4 * 3 > IMAGE_CAP {
        return None;
    }
    let mut bytes = Vec::with_capacity(end / 4 * 3 + 2);
    let mut group = [0u32; 4];
    let mut held = 0usize;
    for byte in &raw[..end] {
        group[held] = sextet(*byte)?;
        held += 1;
        if held == 4 {
            bytes.push(((group[0] << 2) | (group[1] >> 4)) as u8);
            bytes.push(((group[1] << 4) | (group[2] >> 2)) as u8);
            bytes.push(((group[2] << 6) | group[3]) as u8);
            held = 0;
        }
    }
    match held {
        0 => {}
        2 => bytes.push(((group[0] << 2) | (group[1] >> 4)) as u8),
        3 => {
            bytes.push(((group[0] << 2) | (group[1] >> 4)) as u8);
            bytes.push(((group[1] << 4) | (group[2] >> 2)) as u8);
        }
        // `end % 4 == 1` was refused above; anything else is a bug in this function.
        _ => return None,
    }
    Some(bytes)
}

/// One base64 character's six bits.
fn sextet(byte: u8) -> Option<u32> {
    Some(match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
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

    /// One image block of a result, with `bytes` base64-encoded the same way pi encodes it.
    fn image(index: usize, mime: &str, bytes: &[u8]) -> Image {
        Image {
            index,
            mime: mime.to_string(),
            data: encode_base64(bytes),
        }
    }

    /// The encoder the tests need and the build does not: a body has to be written the way pi writes it, and
    /// the decoder is the half under test. Short enough that a vector could not hide a mistake.
    fn encode_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for group in bytes.chunks(3) {
            let mut held = [0u32; 3];
            for (slot, byte) in group.iter().enumerate() {
                held[slot] = u32::from(*byte);
            }
            let packed = (held[0] << 16) | (held[1] << 8) | held[2];
            for place in 0..4 {
                if place <= group.len() {
                    let sextet = (packed >> (18 - 6 * place)) & 0x3f;
                    out.push(ALPHABET[sextet as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn base64_is_decoded_and_anything_that_is_not_base64_is_refused() {
        // The vectors every base64 has: the padding rules, the two uncovered trailing shapes, and `+`/`/`.
        assert_eq!(decode_base64(""), Some(Vec::new()));
        assert_eq!(decode_base64("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(decode_base64("aGVsbG8h"), Some(b"hello!".to_vec()));
        assert_eq!(decode_base64("/w=="), Some(vec![0xff]));
        assert_eq!(decode_base64("AA=="), Some(vec![0x00]));
        // Padding left off still decodes: a producer that omits it is not producing something else.
        assert_eq!(decode_base64("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(decode_base64("aGVsbA"), Some(b"hell".to_vec()));
        for bad in [
            "a",
            "abcde",
            "aGVsbG8-",
            "aGVs bG8=",
            "a=VsbG8=",
            "=",
            "==",
            "aGVsbG8=a",
            "aGVsbG8==",
            "caf\u{e9}",
        ] {
            assert!(
                decode_base64(bad).is_none(),
                "{bad:?} decoded: {:?}",
                decode_base64(bad)
            );
        }
        // A hostile body is refused from its length, before a gigabyte is allocated for it.
        let huge = "A".repeat(IMAGE_CAP / 3 * 4 + 8);
        assert!(decode_base64(&huge).is_none());
    }

    #[test]
    fn an_image_is_stored_beside_the_text_and_served_with_its_own_type() {
        let scratch = Scratch::new("image");
        let store = Store::new(&scratch.0);
        let png = b"\x89PNG\r\n\x1a\nnot really, but the bytes are what matters";
        let stored = store.store_images(SESSION, TOOL, &[image(1, "image/png", png)]);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].index, 1);
        assert_eq!(stored[0].mime, "image/png");
        assert_eq!(
            stored[0].reference,
            format!("/api/sessions/{SESSION}/tool-image/{TOOL}/1")
        );
        // The name says what it is, and the bytes are the image rather than the base64 of it.
        let path = scratch
            .0
            .join("sessions")
            .join(SESSION)
            .join("tool")
            .join(format!("{TOOL}-1.png"));
        assert_eq!(std::fs::read(&path).expect("the file"), png);
        assert_eq!(
            store.read_image(SESSION, TOOL, 1),
            Some((png.to_vec(), "image/png"))
        );
        // The text file is a sibling and neither name can be mistaken for the other.
        let text = store
            .store(SESSION, TOOL, "Read image file [image/png]")
            .expect("stored");
        assert_eq!(text.preview, "Read image file [image/png]");
        assert!(store.path(SESSION, TOOL).expect("a path").is_file());
    }

    #[test]
    fn an_image_type_this_build_cannot_name_is_kept_as_bytes_and_the_frame_still_says_what_it_was()
    {
        let scratch = Scratch::new("image-unnamed");
        let store = Store::new(&scratch.0);
        let stored = store.store_images(SESSION, TOOL, &[image(0, "image/avif", b"avif bytes")]);
        assert_eq!(stored.len(), 1, "a picture is worth keeping even unnamed");
        assert_eq!(
            stored[0].mime, "image/avif",
            "the frame keeps pi's own word"
        );
        assert_eq!(
            store.read_image(SESSION, TOOL, 0),
            Some((b"avif bytes".to_vec(), "application/octet-stream")),
            "the served type is what the file can honestly claim"
        );
    }

    #[test]
    fn an_image_that_is_not_base64_or_is_over_the_cap_is_not_stored_and_not_referenced() {
        let scratch = Scratch::new("image-refused");
        let store = Store::new(&scratch.0);
        let broken = Image {
            index: 0,
            mime: "image/png".to_string(),
            data: "not base64 at all!".to_string(),
        };
        assert!(
            store.store_images(SESSION, TOOL, &[broken]).is_empty(),
            "a refused image must not leave a reference behind"
        );
        assert!(store.read_image(SESSION, TOOL, 0).is_none());
        // An id that cannot be a filename writes nothing, and neither does one image among good ones.
        assert!(
            store
                .store_images(SESSION, "../app", &[image(0, "image/png", b"x")])
                .is_empty()
        );
        let mixed = [
            image(0, "image/png", b"good"),
            Image {
                index: 1,
                mime: "image/png".to_string(),
                data: "=".to_string(),
            },
        ];
        let stored = store.store_images(SESSION, TOOL, &mixed);
        assert_eq!(stored.len(), 1, "the good one is still stored: {stored:?}");
        assert_eq!(stored[0].index, 0);
    }

    #[test]
    fn a_second_result_replaces_the_first_images_type_as_well_as_its_bytes() {
        let scratch = Scratch::new("image-again");
        let store = Store::new(&scratch.0);
        store.store_images(SESSION, TOOL, &[image(0, "image/png", b"first")]);
        store.store_images(SESSION, TOOL, &[image(0, "image/gif", b"second")]);
        assert_eq!(
            store.read_image(SESSION, TOOL, 0),
            Some((b"second".to_vec(), "image/gif")),
            "the read side tries png first, so the stale png had to go"
        );
    }

    #[test]
    fn an_image_nobody_stored_is_no_bytes_at_all() {
        let scratch = Scratch::new("image-missing");
        let store = Store::new(&scratch.0);
        assert!(store.read_image(SESSION, TOOL, 0).is_none());
        assert!(store.image("../../etc", TOOL, 0).is_none());
        assert!(store.image(SESSION, "../app", 0).is_none());
    }
}
