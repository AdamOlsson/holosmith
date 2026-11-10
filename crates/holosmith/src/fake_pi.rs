//! The fake pi: a container's `pi` that replays a recorded turn (HS-077, spec item 10 step 2).
//!
//! Item 10's second step must run a **successful** turn against the real daemon with no provider key on the machine,
//! and the build has no such turn in it: a session's container is on an `--internal` network with a dummy key, so pi
//! refuses `prompt` before a stream exists — that refusal is what `case_docker_turn` measures, and `docs/build.md`'s
//! V3 procedure is the by-hand run that needs a real key. What makes a *successful* turn measurable by a robot is
//! this module: a program that speaks pi's side of the RPC protocol (`docs/rpc.md`) into the launch's two pipes and
//! prints a turn that was recorded once, from a real one.
//!
//! **The recording is HS-032's, byte for byte.** `fixtures/smoke-turn.jsonl` is one header line and then the 69
//! records of `tests/fixtures/pi-turn-tool.jsonl`: `agent_start`, deltas, one tool round trip, non-zero usage and a
//! closing `agent_settled`. The header carries the fixture's own version and **the schema it was recorded against**,
//! and a test compares that number with [`crate::schema::SCHEMA_VERSION`], so a schema change that arrives without a
//! decision about this fixture is a failure with both numbers in it rather than a surprise at a promotion.
//!
//! **It is embedded, not read.** The container runs the *candidate* binary, and the candidate is what must have the
//! recording: `include_str!` puts it in the artifact (ADR-0080's rule for recipes, for the same reason — a release has
//! no `fixtures/` beside it).
//!
//! **Two halves, and the reader's rules are the contract.**
//!
//! * A command in, a `response` out, with the **incoming** id echoed. [`crate::agent`] routes `response` records by
//!   command id, so the recorded id (`req-1`) is not usable: it belongs to a command this client never sent. Every
//!   command is answered — `get_state`, `prompt`, `abort`, `steer`, `clear_queue`, and anything a later pi adds —
//!   because a command answered with nothing is a `COMMAND_TIMEOUT` inside a *different* story's path (the delete's
//!   abort, the ladder's rungs), which is a hang in the wrong place.
//! * The recording follows the `prompt` answer, minus its own `response` line: that line is pi's answer to *that*
//!   recording's command, and the id above replaces it.
//! * EOF is an orderly end: closing stdin is what pi treats as shutdown, so this exits 0 and the exit classifier sees
//!   a clean stop rather than a failure.
//!
//! **`--version` is the pinned one.** The image's `pi` is this binary, so `agent::version_of` asking the image which
//! pi it runs gets [`crate::pins::pi_version`] — the check stays *measured* through the real launch path instead of
//! being skipped in the smoke run, which is the whole point of putting the fake behind pi's name rather than beside it.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The recording: one header line, then HS-032's tool turn verbatim.
pub const FIXTURE: &str = include_str!("../fixtures/smoke-turn.jsonl");

/// The fixture format's own version — what a line of the file means, not what the database is.
pub const FIXTURE_VERSION: u32 = 1;

/// The header line's shape: what the file is, and what it was recorded against.
///
/// Serialized as well as deserialized because a test writes a synthetic header (a future one, an older one) and
/// because the committed file's first line is asserted to round-trip: a header the code cannot read back is a file
/// nobody can keep correct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// Always `"fixture"`: the line is distinguishable from a record, which always has a `type` pi knows.
    #[serde(rename = "type")]
    pub kind: String,
    /// [`FIXTURE_VERSION`].
    pub version: u32,
    /// The **database** schema this recording was made against ([`crate::schema::SCHEMA_VERSION`]).
    pub schema: u32,
    /// Which story recorded it. Free prose: it is for a reader, not for a machine.
    pub recorded: String,
    /// Which of HS-032's turns it is (`text`, `thinking`, `tool`, `image`, `extension-ui`).
    pub turn: String,
}

/// Why the replay could not be set up, or the schema claim was not met.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureError {
    /// The file is there but its header is not the shape [`Header`] describes.
    Header(String),
    /// The header's `schema` is **newer** than this binary's: a recording from a release that knows more than this
    /// build does, which this build cannot promise to replay.
    Newer { fixture: u32, binary: u32 },
}

impl std::fmt::Display for FixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Header(problem) => write!(
                f,
                "the committed turn fixture's first line is not a fixture header ({problem}): \
                 fixtures/smoke-turn.jsonl must begin with {{\"type\":\"fixture\",…}} and the records follow"
            ),
            Self::Newer { fixture, binary } => write!(
                f,
                "the committed turn fixture was recorded against schema {fixture}, and this binary's schema is \
                 {binary}: a newer recording is one this build cannot promise to replay — rebuild this binary from \
                 a release that has it, or re-record the fixture on this one"
            ),
        }
    }
}

/// The header, read from [`FIXTURE`]'s first line.
pub fn header() -> Result<Header, FixtureError> {
    let line = FIXTURE
        .lines()
        .next()
        .ok_or_else(|| FixtureError::Header("the file is empty".to_string()))?;
    let header: Header =
        serde_json::from_str(line).map_err(|error| FixtureError::Header(error.to_string()))?;
    if header.kind != "fixture" {
        return Err(FixtureError::Header(format!(
            "its type is {:?}, not \"fixture\"",
            header.kind
        )));
    }
    Ok(header)
}

/// Why this build may or may not replay the fixture, and the one-line note when it is an older recording.
///
/// **A newer recording is refused, an older one is replayed.** The rule is not symmetry for its own sake: the
/// recording is *in this binary*, so a header from the future is a hand-built or mis-merged artifact, and replaying
/// records a build has no mapping for would produce a turn whose frames are quietly missing. Older is the ordinary
/// case on the release after a schema bump — `rpc::Mapping` counts an unknown record rather than dying (ADR-0040) —
/// so it replays and says so once, on stderr, where a promotion's log keeps it.
pub fn check(header: &Header) -> Result<Option<String>, FixtureError> {
    let binary = crate::schema::SCHEMA_VERSION;
    match header.schema.cmp(&binary) {
        std::cmp::Ordering::Greater => Err(FixtureError::Newer {
            fixture: header.schema,
            binary,
        }),
        std::cmp::Ordering::Equal => Ok(None),
        std::cmp::Ordering::Less => Ok(Some(format!(
            "the committed turn fixture was recorded against schema {} and this binary's is {binary}: it is \
             replayed, because a record this build cannot map is counted rather than fatal, and a fixture is \
             re-recorded when the mapping changes rather than when the database does",
            header.schema
        ))),
    }
}

/// Every record line the fixture holds, the header and the recorded `prompt` answer left out.
///
/// The header is metadata and the recorded answer is pi's answer to a command whose id this run does not have; both
/// are removed here, once, so [`replay`] is about the protocol and not about the file's shape.
pub fn records() -> Result<Vec<String>, FixtureError> {
    let mut lines = FIXTURE.lines();
    lines
        .next()
        .ok_or_else(|| FixtureError::Header("the file holds no header line".to_string()))?;
    let mut records: Vec<String> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        if records.is_empty() {
            // The recording's own `response` — the answer to the `prompt` that produced it.
            if serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| {
                    value
                        .get("type")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .as_deref()
                == Some("response")
            {
                continue;
            }
        }
        records.push(line.to_string());
    }
    Ok(records)
}

/// Replay the fixture into `out` for every command `commands` carries, and answer how many records were written.
///
/// One command at a time, in the order they arrive (pi answers asynchronously; a client that correlates by id does
/// not care, and a replay that answered in a different order would be a second protocol). A line that is not JSON,
/// or that carries no `id`, is skipped with a `warn`-free silence: it is not a command this protocol has, and pi
/// would answer it with its own error, which is not what this module is for.
pub fn replay<R: BufRead, W: Write>(mut commands: R, mut out: W) -> io::Result<usize> {
    let header =
        header().map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let note = check(&header)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if let Some(note) = note {
        eprintln!("holosmith: container fake-pi: {note}");
    }
    let records =
        records().map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let mut written = 0;
    let mut line = String::new();
    loop {
        line.clear();
        if commands.read_line(&mut line)? == 0 {
            return Ok(written);
        }
        let Ok(command) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = command.get("id").and_then(Value::as_str) else {
            continue;
        };
        let kind = command
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        write_line(&mut out, &answer(id, kind))?;
        written += 1;
        if kind == "prompt" {
            for record in &records {
                out.write_all(record.as_bytes())?;
                out.write_all(b"\n")?;
                written += 1;
            }
            out.flush()?;
        }
    }
}

/// One `response` record for the command `id` of `kind`.
///
/// The payloads are pi's documented ones where the client reads them: `get_state`'s answer is what the ready
/// handshake checks (`success`), and the prompt's `disposition` is what the backend logs. Nothing else is invented,
/// because nothing else is read.
fn answer(id: &str, kind: &str) -> Value {
    let data = match kind {
        "get_state" => json!({ "isStreaming": false }),
        "prompt" => json!({ "disposition": "started" }),
        _ => json!({}),
    };
    json!({ "id": id, "type": "response", "command": kind, "success": true, "data": data })
}

/// One JSONL record, LF-terminated — the protocol's only separator (`docs/rpc.md`).
fn write_line<W: Write>(out: &mut W, value: &Value) -> io::Result<()> {
    out.write_all(serde_json::to_string(value).unwrap_or_default().as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Run the replay over `input` and answer what it printed, line by line.
    fn ran(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        replay(Cursor::new(input.as_bytes()), &mut out).expect("the replay answers");
        String::from_utf8(out)
            .expect("the replay writes UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line is a record"))
            .collect()
    }

    #[test]
    fn the_header_is_the_file_version_and_the_schema_it_was_recorded_against() {
        let header = header().expect("the committed fixture has a header");
        assert_eq!(header.kind, "fixture");
        assert_eq!(header.version, FIXTURE_VERSION);
        assert_eq!(
            header.schema,
            crate::schema::SCHEMA_VERSION,
            "the committed fixture says it was recorded against schema {} and this binary's schema is {}: \
             re-record it (docs/build.md) or say in the header why the mapping is unaffected",
            header.schema,
            crate::schema::SCHEMA_VERSION
        );
        assert_eq!(header.recorded, "HS-032");
        assert_eq!(header.turn, "tool");
        // An older recording replays with one note; a newer one is refused with both numbers in the sentence.
        let older = Header {
            schema: crate::schema::SCHEMA_VERSION - 1,
            ..header.clone()
        };
        assert!(check(&older).expect("older is replayable").is_some());
        let newer = Header {
            schema: crate::schema::SCHEMA_VERSION + 1,
            ..header
        };
        let FixtureError::Newer { fixture, binary } = check(&newer).expect_err("newer is refused")
        else {
            panic!("a newer fixture answered something other than Newer");
        };
        assert_eq!(fixture, crate::schema::SCHEMA_VERSION + 1);
        assert_eq!(binary, crate::schema::SCHEMA_VERSION);
    }

    /// The claim "recorded from a real turn" is only true while the records are the recording's: this compares the
    /// committed fixture's body with HS-032's own file, byte for byte. A hand edit fails here, naming both files.
    #[test]
    fn the_body_is_hs_032_s_recording_verbatim() {
        let recorded = include_str!("../tests/fixtures/pi-turn-tool.jsonl");
        let body: Vec<&str> = FIXTURE.lines().skip(1).collect();
        let original: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            body.len(),
            original.len(),
            "fixtures/smoke-turn.jsonl holds {} records and tests/fixtures/pi-turn-tool.jsonl holds {}: the smoke \
             fixture must be that recording plus a header, so a record was added or removed by hand",
            body.len(),
            original.len()
        );
        for (index, (mine, theirs)) in body.iter().zip(original.iter()).enumerate() {
            assert_eq!(
                mine,
                theirs,
                "record {} of fixtures/smoke-turn.jsonl differs from HS-032's tests/fixtures/pi-turn-tool.jsonl",
                index + 1
            );
        }
    }

    #[test]
    fn the_records_are_the_recording_without_its_header_and_its_own_answer() {
        let records = records().expect("the fixture parses");
        // 70 lines in the file: the header and the recording's own `prompt` answer are the two that are dropped.
        assert_eq!(
            records.len(),
            68,
            "one record per line after the header, minus the recorded answer"
        );
        assert!(
            !records
                .iter()
                .any(|record| record.contains("\"type\":\"fixture\"")),
            "the header was replayed as a record"
        );
        assert!(
            !records[0].contains("\"id\":\"req-1\""),
            "the recorded `prompt` answer was replayed: its id belongs to a command this run never sent"
        );
        assert_eq!(
            records.last().map(String::as_str),
            Some("{\"type\":\"agent_settled\",\"aborted\":false}"),
            "the turn must end where pi says it has no work left"
        );
    }

    /// One command in, one answer out — with the **incoming** id, which is the whole reason the recorded one is
    /// dropped: `agent::request` waits for the id it sent.
    #[test]
    fn every_command_is_answered_with_the_id_that_was_sent() {
        let records = ran(
            "{\"id\":\"ready-1\",\"type\":\"get_state\"}\n{\"id\":\"turn-7\",\"type\":\"prompt\",\"message\":\"hi\"}\n{\"id\":\"a-1\",\"type\":\"abort\"}\n{\"id\":\"c-1\",\"type\":\"clear_queue\"}\n{\"id\":\"s-1\",\"type\":\"steer\",\"message\":\"x\"}\n",
        );
        let answers: Vec<&Value> = records
            .iter()
            .filter(|record| record["type"] == "response")
            .collect();
        let ids: Vec<&str> = answers
            .iter()
            .map(|answer| answer["id"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(ids, ["ready-1", "turn-7", "a-1", "c-1", "s-1"]);
        assert_eq!(answers[0]["command"], "get_state");
        assert!(answers[0]["success"].as_bool().unwrap_or(false));
        assert_eq!(answers[0]["data"]["isStreaming"], false);
        assert_eq!(answers[1]["data"]["disposition"], "started");
        assert!(
            !records.iter().any(|record| record["id"] == "req-1"),
            "the recorded id answered something"
        );
    }

    /// The turn itself: deltas, one tool round trip and usage — the three facts the acceptance names, counted over
    /// the records rather than over the mapping (the mapping is `tests/fake_pi.rs`'s subject).
    #[test]
    fn a_prompt_replays_a_turn_with_deltas_a_tool_call_and_usage() {
        let records = ran("{\"id\":\"turn-1\",\"type\":\"prompt\",\"message\":\"hi\"}\n");
        let count = |kind: &str| {
            records
                .iter()
                .filter(|record| record["type"] == kind)
                .count()
        };
        assert!(count("message_update") >= 10, "no deltas arrived");
        assert_eq!(count("tool_execution_start"), 1, "one tool call starts");
        assert_eq!(count("tool_execution_end"), 1, "and ends");
        assert_eq!(
            records.last().map(|record| record["type"].clone()),
            Some(json!("agent_settled")),
            "the turn did not settle"
        );
        let usage = records
            .iter()
            .filter_map(|record| record["message"].get("usage"))
            .filter_map(|usage| usage.get("totalTokens").and_then(Value::as_u64))
            .max()
            .expect("the recording reports usage");
        assert!(usage > 0, "the recorded usage is all zeroes");
    }

    /// EOF is an orderly end, not an error: a delete closes stdin, and the exit classifier must see 0.
    #[test]
    fn an_empty_stdin_is_a_clean_stop_with_nothing_written() {
        let mut out = Vec::new();
        assert_eq!(
            replay(Cursor::new(b""), &mut out).expect("EOF is not an error"),
            0
        );
        assert!(out.is_empty());
    }
}
