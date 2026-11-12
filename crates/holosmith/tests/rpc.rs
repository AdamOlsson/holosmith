//! pi's records become the transcript's frames.
//!
//! Two things are proven here, and they are different questions. The **fixtures** are real turns: the agent
//! image (`pi-agent:local`, whose pi is `pins/pi.txt`) was run with the operator's own provider credentials and
//! one prompt, and every record it printed was committed verbatim. Replaying them through `rpc::Mapping` shows
//! the contract this build writes for records no test wrote by hand — including the ones the mapping must
//! *decline* (the system prompt, the user's own message, pi's bookkeeping).
//!
//! The **pump** is the end-to-end half: a fake agent process prints the fixture on stdout, and the frames it
//! produces are read back out of a real event log, in order, with the ids the mapping minted. That is the
//! acceptance's "every delta reaches the writer; ordering is preserved end to end" — the container's half of it
//! is the gate's `case_docker_turn`, which prompts a real pi.
//!
//! Nothing here needs a daemon: the fixtures are committed, and the fake agent is a shell script. A test that
//! needed Docker would be a gate case instead.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use holosmith::agent::{Agent, Received};
use holosmith::db::{Database, OpenOptions};
use holosmith::events::{
    self, Event, MESSAGE_DELTA, MESSAGE_END, TOOL_END, TOOL_START, TURN_ERROR, WriterOptions,
};
use holosmith::rpc::{Mapped, Mapping, ToolEnd, pump};
use holosmith::sessions::{NewSession, Session};
use holosmith::tool::{Store, truncated_marker};

mod support;
use support::TempDir;

/// The turn that answered with text and nothing else.
const TEXT: &str = include_str!("fixtures/pi-turn-text.jsonl");

/// The turn that thought before it answered.
const THINKING: &str = include_str!("fixtures/pi-turn-thinking.jsonl");

/// The turn that called a tool: pi's `toolcall_*` (the provider's streaming of the call) and
/// `tool_execution_*` (the call's run and its result), which is what maps and files.
const TOOL: &str = include_str!("fixtures/pi-turn-tool.jsonl");

/// The turn whose tool result carried an **image**: `read` on a four-pixel PNG, whose
/// `tool_execution_end.result.content` holds a text block and an `image` block with the picture in base64.
const IMAGE: &str = include_str!("fixtures/pi-turn-image.jsonl");

/// The turn a project-local extension asked a question in: one `select` dialog, which pi blocks on
/// until the client answers, and one `notify` afterwards whose text records what the dialog returned.
const EXTENSION_UI: &str = include_str!("fixtures/pi-turn-extension-ui.jsonl");

/// The instant every row this test writes carries, so a timestamp is asserted rather than raced.
const NOW: i64 = 1_760_000_000;

/// How long a test waits for the writer thread before calling it stuck.
const PATIENCE: Duration = Duration::from_secs(10);

/// One frame, as the mapping produced it.
#[derive(Debug, Clone)]
struct Frame {
    kind: String,
    payload: Value,
}

/// The fixture's records, parsed the way the reader hands them over.
fn records(fixture: &str) -> Vec<Value> {
    fixture
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("every fixture line is JSON"))
        .collect()
}

/// Every frame `fixture` maps to, and what the mapping declined.
fn mapped(fixture: &str) -> (Vec<Frame>, Vec<bool>, u64, u64) {
    let mut mapping = Mapping::new("0123456789abcdef");
    let mut frames = Vec::new();
    let mut settled = Vec::new();
    for record in records(fixture) {
        match mapping.map(&record) {
            Mapped::Frame { kind, payload } => frames.push(Frame {
                kind: kind.to_string(),
                payload: serde_json::from_str(&payload).expect("a frame's payload is JSON"),
            }),
            Mapped::Settled { aborted } => settled.push(aborted),
            // A tool's result is a frame whose whole output belongs in a file. With no data root there is
            // none, so the frame is the previewed shape — the served path stores first (see the pump tests).
            mapped => {
                if let Some((kind, payload)) = mapped.into_frame() {
                    frames.push(Frame {
                        kind: kind.to_string(),
                        payload: serde_json::from_str(&payload).expect("a frame's payload is JSON"),
                    });
                }
            }
        }
    }
    (frames, settled, mapping.skipped(), mapping.unknown())
}

/// Print the frames a fixture maps to, one line each.
///
/// Written to stderr, so a normal `cargo test` captures it and `-- --nocapture` shows it: the demo's "a real
/// turn, in order, with no duplication" is this output, produced by the assertions' own code path.
fn print_frames(label: &str, frames: &[Frame]) {
    eprintln!("frame {label}: {} frames", frames.len());
    for frame in frames {
        match frame.payload.get("text") {
            Some(text) => eprintln!(
                "frame {} kind={} text={:?}",
                frame.kind, frame.payload["kind"], text
            ),
            None => eprintln!("frame {} {}", frame.kind, frame.payload),
        }
    }
}

/// The frames of one kind, in order.
fn of_kind<'a>(frames: &'a [Frame], kind: &str) -> Vec<&'a Frame> {
    frames.iter().filter(|frame| frame.kind == kind).collect()
}

/// The text of one delta kind, concatenated in order: what the transcript's block will hold.
fn text_of(frames: &[Frame], kind: &str) -> String {
    of_kind(frames, MESSAGE_DELTA)
        .into_iter()
        .filter(|frame| frame.payload["kind"] == kind)
        .map(|frame| {
            frame.payload["text"]
                .as_str()
                .expect("a delta's text")
                .to_string()
        })
        .collect()
}

#[test]
fn the_recorded_text_turn_becomes_the_contracts_frames_in_order() {
    let (frames, settled, skipped, unknown) = mapped(TEXT);
    print_frames("text", &frames);
    let kinds: Vec<&str> = frames.iter().map(|frame| frame.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [MESSAGE_DELTA, MESSAGE_END],
        "one text block and its end: the turn's boundary pair is `pump`'s, because the contract's turn is the \
         prompt and pi's `turn_start`/`turn_end` are per assistant response: {kinds:?}"
    );
    assert_eq!(settled, vec![false], "the agent settled, and did not abort");
    assert_eq!(
        (skipped, unknown),
        (7, 0),
        "pi's system prompt, its user message, its boundaries, its prompt's `response` and its bookkeeping: \
         seven documented records this contract has no frame for, and no record type this build has never seen"
    );

    assert_eq!(text_of(&frames, "text"), "hello world");
    assert_eq!(text_of(&frames, "thinking"), "");

    let end = of_kind(&frames, MESSAGE_END)[0];
    assert_eq!(
        end.payload["message_id"], frames[0].payload["message_id"],
        "the message's deltas and its end share the id the mapping minted"
    );
    assert_eq!(
        end.payload["usage"],
        json!({"in": 1631, "out": 4}),
        "991 prompt tokens plus 640 the provider served from its cache: what the message cost, which \
         is what the budget counts (ADR-0042)"
    );
    assert_eq!(end.payload["stop_reason"], "stop");
    assert_eq!(
        end.payload["tool_calls"],
        json!([]),
        "this turn called no tool"
    );
}

#[test]
fn the_recorded_thinking_turn_streams_thinking_before_text() {
    let (frames, settled, _, unknown) = mapped(THINKING);
    print_frames("thinking", &frames);
    assert_eq!(settled, vec![false]);
    assert_eq!(unknown, 0, "pi's thinking records are documented ones");

    let delta_kinds: Vec<&str> = of_kind(&frames, MESSAGE_DELTA)
        .into_iter()
        .map(|frame| frame.payload["kind"].as_str().expect("a delta's kind"))
        .collect();
    assert_eq!(
        delta_kinds,
        ["thinking", "thinking", "text"],
        "pi thought twice and then answered: {delta_kinds:?}"
    );
    assert!(
        !text_of(&frames, "thinking").is_empty(),
        "the thinking block has the text pi streamed"
    );
    assert!(!text_of(&frames, "text").is_empty());

    let end = of_kind(&frames, MESSAGE_END)[0];
    assert_eq!(end.payload["usage"], json!({"in": 1492, "out": 10}));
    let ids: Vec<&Value> = of_kind(&frames, MESSAGE_DELTA)
        .into_iter()
        .map(|frame| &frame.payload["message_id"])
        .collect();
    assert!(
        ids.iter().all(|id| **id == end.payload["message_id"]),
        "one message, one id: {ids:?}"
    );
}

#[test]
fn every_usage_the_fixtures_report_adds_up_to_pis_own_total() {
    // The arithmetic in `usage()` — a prompt's cached part is part of what it cost — is asserted by pi's own
    // `totalTokens` on every record of every committed turn, rather than by a constant this test could get
    // wrong in the same way the code did. A provider that starts reporting `reasoning` outside `output`, or a
    // cache counter that moves into `input`, fails here and says so.
    let mut checked = 0;
    for (name, fixture) in [("text", TEXT), ("thinking", THINKING), ("tool", TOOL)] {
        for record in records(fixture) {
            if record.get("type").and_then(Value::as_str) != Some("message_end") {
                continue;
            }
            let message = &record["message"];
            let Some(counts) = message.get("usage") else {
                continue;
            };
            let Some(total) = counts.get("totalTokens").and_then(Value::as_u64) else {
                continue;
            };
            let (frames, _, _, _) = mapped(&json!(record).to_string());
            let end = frames
                .iter()
                .find(|frame| frame.kind == MESSAGE_END)
                .unwrap_or_else(|| panic!("{name}: a usage record mapped to no message.end"));
            let counts = &end.payload["usage"];
            let sum = counts["in"].as_u64().expect("in") + counts["out"].as_u64().expect("out");
            assert_eq!(
                sum, total,
                "{name}: the frame's two counts do not add up to pi's totalTokens: {counts} vs {total}"
            );
            checked += 1;
        }
    }
    assert_eq!(
        checked, 4,
        "the fixtures hold four reported usages to check"
    );
}

#[test]
fn a_fixture_whose_records_drifted_fails_this_guard() {
    // The guard that makes the two replays above meaningful, and the one thing they cannot assert: *which*
    // records the fixtures hold. A refresh that dropped pi's system prompt — or that stopped recording the
    // thinking blocks — would leave every mapping assertion above green, because a frame cannot be missing for a
    // record that is not there. So the record types are counted per fixture.
    let multiset = |fixture: &str| {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for record in records(fixture) {
            let kind = record
                .get("type")
                .and_then(Value::as_str)
                .expect("every fixture record names its type");
            match counts.iter_mut().find(|(seen, _)| seen == kind) {
                Some((_, count)) => *count += 1,
                None => counts.push((kind.to_string(), 1)),
            }
        }
        counts
    };
    let expect = |pairs: &[(&str, usize)]| -> Vec<(String, usize)> {
        pairs
            .iter()
            .map(|(kind, count)| (kind.to_string(), *count))
            .collect()
    };
    assert_eq!(
        multiset(TEXT),
        expect(&[
            ("response", 1),
            ("agent_start", 1),
            ("turn_start", 1),
            ("message_start", 3),
            ("message_end", 3),
            ("message_update", 3),
            ("turn_end", 1),
            ("agent_end", 1),
            ("agent_settled", 1),
        ]),
        "the text fixture is a real turn's records, verbatim"
    );
    assert_eq!(
        multiset(THINKING),
        expect(&[
            ("response", 1),
            ("agent_start", 1),
            ("turn_start", 1),
            ("message_start", 3),
            ("message_end", 3),
            ("message_update", 7),
            ("turn_end", 1),
            ("agent_end", 1),
            ("agent_settled", 1),
        ]),
        "the thinking fixture is the same turn shape with a thinking block, and it is the only fixture that \
         exercises that arm"
    );
    assert_eq!(
        multiset(TOOL),
        expect(&[
            ("response", 1),
            ("agent_start", 1),
            // Two assistant responses around one tool call: pi's own boundaries are per response, and this is
            // the fixture that proves the contract's turn is not theirs.
            ("turn_start", 2),
            ("message_start", 5),
            ("message_end", 5),
            ("message_update", 47),
            ("tool_execution_start", 1),
            ("tool_execution_update", 2),
            ("tool_execution_end", 1),
            // The second response's boundary ends after its text, which is why it appears here.
            ("turn_end", 2),
            ("agent_end", 1),
            ("agent_settled", 1),
        ]),
        "the tool fixture is a real turn's records, verbatim: a call's streamed arguments, its run, its result \
         and the answer that repeated what it printed"
    );
    assert_eq!(
        multiset(IMAGE),
        expect(&[
            ("response", 1),
            ("agent_start", 1),
            // Two assistant responses around the call, as in the tool fixture.
            ("turn_start", 2),
            ("message_start", 5),
            ("message_end", 5),
            ("message_update", 23),
            ("tool_execution_start", 1),
            ("tool_execution_end", 1),
            ("turn_end", 2),
            ("agent_end", 1),
            ("agent_settled", 1),
        ]),
        "the image fixture is a real turn's records, verbatim: a call whose result held a text block and an \
         image block, and the answer that described the picture"
    );
    assert_eq!(
        multiset(EXTENSION_UI),
        expect(&[
            ("response", 1),
            ("agent_start", 1),
            // The two requests are the point of the fixture: one dialog (which blocks) and one notify (which
            // does not), so a refresh that dropped either would leave one of the two arms unexercised.
            ("extension_ui_request", 2),
            ("turn_start", 1),
            ("message_start", 3),
            ("message_end", 3),
            ("message_update", 3),
            ("turn_end", 1),
            ("agent_end", 1),
            ("agent_settled", 1),
        ]),
        "the extension fixture is a real turn's records, verbatim: a project-local extension's `select` dialog \
         before the turn, the `notify` that recorded the cancel, and the answer it did not wait for"
    );
}

/// A real event log, its writer, and one session for the frames to hang off.
struct Log {
    database: Arc<Mutex<Database>>,
    writer: events::EventWriter,
    session: Session,
    dir: TempDir,
}

impl Log {
    fn new(dir: TempDir) -> Self {
        let database = Arc::new(Mutex::new(
            Database::open(&dir.database(), OpenOptions::default(), NOW)
                .expect("a fresh database opens"),
        ));
        let (writer, _stream) = events::spawn(
            Arc::clone(&database),
            WriterOptions {
                commit_interval: Duration::from_millis(20),
            },
        );
        let session = database
            .lock()
            .expect("the lock is not poisoned")
            .sessions()
            .create(
                &NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: Some("rpc-test".to_string()),
                    base_commit: None,
                },
                NOW,
            )
            .expect("a session is created");
        Self {
            database,
            writer,
            session,
            dir,
        }
    }

    /// The tool-output store the pump writes to: the same data root, so a test reads back what a turn stored.
    fn store(&self) -> Store {
        Store::new(self.dir.path())
    }

    /// The session log a served run's backend holds, over the same data root: what spec item 15 forbids a
    /// tool's output from reaching.
    fn sink(&self) -> holosmith::exit::SessionLog {
        holosmith::exit::SessionLog::new(self.dir.path())
    }

    /// Every committed event of the session, in seq order, once `count` of them are there.
    fn committed(&self, count: usize) -> Vec<Event> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let events = self.database.lock().expect("the lock").events().read_since(
                &self.session.id,
                0,
                // A window wide enough that the test's own `count` is never the truncation.
                10_000,
            );
            match events {
                Ok(events) if events.len() >= count => return events,
                Ok(_) => {}
                Err(error) => panic!("the log could not be read: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "the writer committed fewer than {count} events"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A shell script that answers one prompt and then prints `fixture` on stdout.
///
/// The records are written in one burst, which is what makes the coalescing assertion below exact: one commit
/// window merges a message's consecutive deltas (ADR-0031), so two thinking deltas become one row and the
/// transcript is unchanged either way.
fn fake_agent(dir: &TempDir, fixture: &str) -> std::process::Command {
    let script = dir.write(
        "fake-pi.sh",
        "#!/bin/sh\n\
         # One prompt in, one accepted answer out, then the recorded turn.\n\
         IFS= read -r line\n\
         id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p')\n\
         printf '{\"type\":\"response\",\"id\":\"%s\",\"command\":\"prompt\",\"success\":true,\
         \"data\":{\"disposition\":\"started\"}}\\n' \"$id\"\n\
         cat \"$1\"\n\
         cat > /dev/null\n",
    );
    let recording = dir.write("turn.jsonl", fixture);
    let _ = std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(&script).arg(&recording);
    command
}

/// A shell script that answers one prompt, prints `fixture` on stdout, and then records everything written to
/// its stdin afterwards into `$2`.
///
/// The second half is what makes "the pump answered the dialog" an assertion rather than a hope: pi's dialog
/// methods block until a response arrives, so the only proof that the cancel reached the agent is the bytes on
/// its stdin — and a fake agent that wrote them nowhere would let a pump that answered nothing stay green.
fn fake_agent_recording_stdin(
    dir: &TempDir,
    fixture: &str,
) -> (std::process::Command, std::path::PathBuf) {
    let script = dir.write(
        "fake-pi-recording.sh",
        "#!/bin/sh\n\
         IFS= read -r line\n\
         id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p')\n\
         printf '{\"type\":\"response\",\"id\":\"%s\",\"command\":\"prompt\",\"success\":true,\
         \"data\":{\"disposition\":\"started\"}}\\n' \"$id\"\n\
         cat \"$1\"\n\
         cat > \"$2\"\n",
    );
    let recording = dir.write("turn.jsonl", fixture);
    let captured = dir.path().join("stdin.txt");
    let _ = std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(&script).arg(&recording).arg(&captured);
    (command, captured)
}

#[test]
fn the_recorded_tool_turn_becomes_a_start_a_delta_run_an_end_and_one_tool_pair() {
    // The third arm adds, and the shape it must have: **one** `tool.start`/`tool.end` pair for one
    // call, even though pi printed both the provider's `toolcall_*` stream and the execution's records. The
    // call's id is the same in both, so mapping both would open two blocks for one call (ADR-0041).
    let (frames, settled, skipped, unknown) = mapped(TOOL);
    print_frames("tool", &frames);
    assert_eq!(settled, vec![false]);
    assert_eq!(unknown, 0, "every record in this turn is one pi documents");

    let kinds: Vec<&str> = frames.iter().map(|frame| frame.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            MESSAGE_END,
            TOOL_START,
            TOOL_END,
            MESSAGE_DELTA,
            MESSAGE_DELTA,
            MESSAGE_DELTA,
            MESSAGE_DELTA,
            MESSAGE_END
        ],
        "the first answer asked for a tool, the call ran, then the answer repeated what it printed: {kinds:?}"
    );

    // The call itself: the name and a summary of the arguments, from the execution's own record.
    let start = of_kind(&frames, TOOL_START)[0];
    assert_eq!(start.payload["tool_id"], "call_00_9hlt8we6459oa420spsp3mju");
    assert_eq!(start.payload["name"], "bash");
    let arguments = start.payload["arguments"].as_str().expect("arguments");
    assert!(
        arguments.contains("hello from the tool"),
        "the arguments summary carries the command: {arguments}"
    );

    // Its result: the output, the tool's own verdict and pi's duration.
    let end = of_kind(&frames, TOOL_END)[0];
    assert_eq!(end.payload["tool_id"], start.payload["tool_id"]);
    assert_eq!(end.payload["name"], "bash");
    assert_eq!(end.payload["output"], "hello from the tool\n");
    assert_eq!(end.payload["status"], "ok");
    assert!(
        end.payload["duration_ms"].as_u64().is_some(),
        "pi measured the tool: {}",
        end.payload["duration_ms"]
    );
    assert!(
        end.payload.get("output_ref").is_none(),
        "a replay with no data root stores nothing, so it names no reference: {}",
        end.payload
    );
    // A turn with a tool call declines more records than a plain one — pi's streamed call arguments and the
    // tool's partial results — and the count is the drift guard's business, not this test's.
    assert!(skipped > 0);
}

/// One tool turn whose `bash` printed `bytes` bytes of `#`: bigger than the preview, small enough to assert.
fn tool_turn(bytes: usize) -> String {
    [
        json!({"type": "agent_start"}).to_string(),
        json!({"type": "message_start", "message": {"role": "assistant", "content": []}})
            .to_string(),
        json!({"type": "tool_execution_start", "toolCallId": TOOL_ID, "toolName": "bash",
               "args": {"command": "head -c 4096 /dev/zero | tr '\\0' '#'"}})
        .to_string(),
        json!({"type": "tool_execution_end", "toolCallId": TOOL_ID, "toolName": "bash",
               "result": {"content": [{"type": "text", "text": "#".repeat(bytes)}]},
               "isError": false, "durationMs": 7})
        .to_string(),
        json!({"type": "agent_settled", "aborted": false}).to_string(),
    ]
    .join("\n")
        + "\n"
}

/// The id the synthetic tool turn uses.
const TOOL_ID: &str = "call_00_9hlt8we6459oa420spsp3mju";

#[tokio::test]
async fn the_pump_files_a_tool_output_so_the_log_carries_only_a_preview() {
    let dir = TempDir::new("rpc-tool");
    let log = Log::new(dir);
    let command = fake_agent(&log.dir, &tool_turn(4_096));
    let (agent, _exit) = Agent::launch(command, &log.session.id).expect("the fake agent starts");
    let mut records = agent.records();
    let reply = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await
        .expect("the fake agent answers");
    assert!(reply.success);

    let mut mapping = Mapping::new(&log.session.id);
    let ended = pump(
        &mut records,
        &mut mapping,
        &log.writer,
        &log.store(),
        &agent,
    )
    .await
    .expect("the fixture settles the turn");
    assert_eq!(ended, holosmith::backend::TurnEnd::Settled);

    // Read the tool's frame back out of the committed log: `tool.end` must name the file, and its own
    // `output` must be the transcript's preview rather than the whole thing.
    let deadline = Instant::now() + PATIENCE;
    let events = loop {
        let events = log
            .database
            .lock()
            .expect("the lock")
            .events()
            .read_since(&log.session.id, 0, 10_000)
            .expect("the log reads");
        if events.iter().any(|event| event.kind == "turn.end") {
            break events;
        }
        assert!(Instant::now() < deadline, "the turn never ended");
        std::thread::sleep(Duration::from_millis(10));
    };
    let payload_of = |kind: &str| -> Value {
        let event = events
            .iter()
            .find(|event| event.kind == kind)
            .unwrap_or_else(|| panic!("no {kind} in the log"));
        serde_json::from_str(&event.payload).expect("a payload is JSON")
    };
    let tool_end = payload_of("tool.end");
    let reference = tool_end["output_ref"].as_str().expect("the reference");
    assert_eq!(
        reference,
        format!("/api/sessions/{}/tool-output/{TOOL_ID}", log.session.id)
    );

    // The file the reference names holds the whole 4096 bytes; the frame holds the preview of it. The output is
    // deliberately longer than `PREVIEW_MAX`, so this cannot pass with the previewing removed.
    let store = log.store();
    let path = store
        .path(&log.session.id, TOOL_ID)
        .expect("the file's path");
    assert!(path.starts_with(log.dir.path()), "{}", path.display());
    let file = std::fs::read_to_string(&path).expect("the file");
    assert_eq!(file.len(), 4_096);
    assert_eq!(file, "#".repeat(4_096));
    let preview = tool_end["output"].as_str().expect("the preview");
    assert_eq!(
        preview.chars().count(),
        holosmith::tool::PREVIEW_MAX + 1,
        "the frame's output is the preview, ellipsis included"
    );
    assert!(preview.ends_with(holosmith::tool::ELLIPSIS));
    assert!(
        file.len() > preview.len(),
        "the file must hold more than the frame does, or this test proves nothing"
    );

    // And the session's own log is stderr's alone (spec item 15). The log is given a line first, so the
    // assertion below is about a file that exists: an `if let Ok(...)` over a file nobody created would pass on
    // every run and prove nothing (this story's review caught exactly that).
    let session_log = log
        .dir
        .path()
        .join("sessions")
        .join(&log.session.id)
        .join("session.log");
    log.sink().append(
        &log.session.id,
        "the agent's stderr, which is what this file is for",
    );
    let text = std::fs::read_to_string(&session_log).expect("the session log exists");
    assert!(
        text.contains("the agent's stderr"),
        "the log is the file this test means to read: {text}"
    );
    assert!(
        !text.contains("hello from the tool"),
        "a tool's output reached session.log: {text}"
    );
}

#[tokio::test]
async fn a_tool_that_prints_megabytes_is_capped_with_a_marker_and_previewed() {
    // The story's own case, at the served path rather than in the store's unit tests: the file is capped and
    // says where it stopped, and the frame is small however large the tool's output was.
    const HUGE: usize = 6 * 1024 * 1024;
    let dir = TempDir::new("rpc-huge");
    let log = Log::new(dir);
    let printed = "abcdefghij".repeat(HUGE / 10);
    let fixture = [
        json!({"type": "agent_start"}).to_string(),
        json!({"type": "message_start", "message": {"role": "assistant", "content": []}})
            .to_string(),
        json!({"type": "tool_execution_start", "toolCallId": "call_big", "toolName": "bash",
               "args": {"command": "cat big.txt"}})
        .to_string(),
        json!({"type": "tool_execution_end", "toolCallId": "call_big", "toolName": "bash",
               "result": {"content": [{"type": "text", "text": printed}]},
               "isError": false, "durationMs": 12})
        .to_string(),
        json!({"type": "agent_settled", "aborted": false}).to_string(),
    ]
    .join("\n")
        + "\n";
    let command = fake_agent(&log.dir, &fixture);
    let (agent, _exit) = Agent::launch(command, &log.session.id).expect("the fake agent starts");
    let mut records = agent.records();
    let _ = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await;
    let mut mapping = Mapping::new(&log.session.id);
    pump(
        &mut records,
        &mut mapping,
        &log.writer,
        &log.store(),
        &agent,
    )
    .await
    .expect("the turn settles");

    let store = log.store();
    let path = store.path(&log.session.id, "call_big").expect("a path");
    let written = std::fs::read_to_string(&path).expect("the file is valid UTF-8");
    let (body, marker) = written.split_once('\n').unwrap_or_else(|| {
        panic!(
            "a capped file ends with a marker line: {} bytes",
            written.len()
        )
    });
    assert_eq!(body.len(), holosmith::tool::CAP, "the body is the cap");
    assert_eq!(
        marker.trim_end(),
        truncated_marker(holosmith::tool::CAP, printed.len()),
        "the marker names what was kept and what there was"
    );

    // The frame is a preview, whatever the tool printed.
    let (frames, _, _, _) = mapped(&fixture);
    let end = of_kind(&frames, TOOL_END)[0];
    let preview = end.payload["output"].as_str().expect("the preview");
    assert!(
        preview.chars().count() <= holosmith::tool::PREVIEW_MAX + 1,
        "the preview is {} characters",
        preview.chars().count()
    );
}

#[tokio::test]
async fn the_pump_writes_the_recorded_turn_into_the_log_in_order() {
    let dir = TempDir::new("rpc-pump");
    let log = Log::new(dir);
    let command = fake_agent(&log.dir, TEXT);
    let (agent, _exit) = Agent::launch(command, &log.session.id).expect("the fake agent starts");

    // The order `start_turn` uses: subscribe first, then write the command, then pump.
    let mut records = agent.records();
    let reply = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await
        .expect("the fake agent answers");
    assert!(reply.success);

    let mut mapping = Mapping::new(&log.session.id);
    let ended = pump(
        &mut records,
        &mut mapping,
        &log.writer,
        &log.store(),
        &agent,
    )
    .await
    .expect("the fixture settles the turn");
    assert_eq!(ended, holosmith::backend::TurnEnd::Settled);
    // The pump's mapping declines the same documented records a direct replay does, minus the fixture's
    // `response` line — the record that answers the prompt, which the reader routes to its caller and never
    // publishes to a turn. Nothing is of an unknown type either way.
    assert_eq!(
        mapping.skipped(),
        mapped(TEXT).2 - 1,
        "the prompt's `response` is routed, not published"
    );
    assert_eq!(mapping.unknown(), 0);

    let events = log.committed(4);
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["turn.start", MESSAGE_DELTA, MESSAGE_END, "turn.end"],
        "the log holds the frames in the order pi printed them: {kinds:?}"
    );

    let payload = |index: usize| -> Value {
        serde_json::from_str(&events[index].payload).expect("a payload is JSON")
    };
    assert_eq!(payload(0), json!({}));
    assert_eq!(payload(1)["kind"], "text");
    assert_eq!(payload(1)["text"], "hello world");
    assert_eq!(payload(2)["usage"], json!({"in": 1631, "out": 4}));
    assert_eq!(payload(2)["message_id"], payload(1)["message_id"]);
    assert_eq!(payload(3), json!({}));

    // The seqs are the writer's and strictly increasing: the order the transcript replays.
    assert!(events.windows(2).all(|pair| pair[0].seq < pair[1].seq));
}

#[tokio::test]
async fn a_coalesced_turn_keeps_its_text_and_its_order() {
    let dir = TempDir::new("rpc-thinking");
    let log = Log::new(dir);
    let command = fake_agent(&log.dir, THINKING);
    let (agent, _exit) = Agent::launch(command, &log.session.id).expect("the fake agent starts");
    let mut records = agent.records();
    agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await
        .expect("the fake agent answers");
    let mut mapping = Mapping::new(&log.session.id);
    pump(
        &mut records,
        &mut mapping,
        &log.writer,
        &log.store(),
        &agent,
    )
    .await
    .expect("the fixture settles the turn");

    let events = log.committed(4);
    let deltas: Vec<Value> = events
        .iter()
        .filter(|event| event.kind == MESSAGE_DELTA)
        .map(|event| serde_json::from_str(&event.payload).expect("a payload is JSON"))
        .collect();
    // Coalescing may fold the two thinking deltas into one row; what must not change is the order of the kinds
    // and the text they carry.
    let kinds: Vec<&str> = deltas
        .iter()
        .map(|delta| delta["kind"].as_str().expect("a kind"))
        .collect();
    assert_eq!(kinds.first(), Some(&"thinking"), "{kinds:?}");
    assert_eq!(kinds.last(), Some(&"text"), "{kinds:?}");
    let thinking: String = deltas
        .iter()
        .filter(|delta| delta["kind"] == "thinking")
        .map(|delta| delta["text"].as_str().expect("text"))
        .collect();
    assert_eq!(thinking, "$0.05", "the whole thought survived coalescing");
    let ids: Vec<&Value> = deltas.iter().map(|delta| &delta["message_id"]).collect();
    assert!(
        ids.windows(2).all(|pair| pair[0] == pair[1]),
        "one message, one id: {ids:?}"
    );
}

#[tokio::test]
async fn an_agent_that_dies_mid_turn_is_a_failed_turn_not_a_hang() {
    // A process that prints one delta and exits: no `agent_settled` ever arrives, so the pump must answer a
    // failure the runner can turn into `unexpected_exit` rather than waiting for ever.
    let dir = TempDir::new("rpc-dies");
    let log = Log::new(dir);
    let script = log.dir.write(
        "dies.sh",
        "#!/bin/sh\n\
         IFS= read -r line\n\
         printf '{\"type\":\"turn_start\"}\n'\n\
         printf '{\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"content\":[]}}\n'\n\
         printf '{\"type\":\"message_update\",\"assistantMessageEvent\":\
         {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"half\"}}\n'\n",
    );
    let _ = std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(&script);

    let (agent, _exit) = Agent::launch(command, &log.session.id).expect("the fake agent starts");
    let mut records = agent.records();
    // The `prompt` write races the process's exit, and either arm is the same answer for this test: the run is
    // over before it settled.
    let _ = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await;
    let mut mapping = Mapping::new(&log.session.id);
    let ended = pump(
        &mut records,
        &mut mapping,
        &log.writer,
        &log.store(),
        &agent,
    )
    .await;
    let error = ended.expect_err("an agent that never settles fails its turn");
    assert!(
        error.to_string().contains("settled"),
        "the reason names the missing settle: {error}"
    );

    // The delta it did print is still in the log: a failed turn is not a silent one. Two rows are there — the
    // turn's opening boundary and that delta — and no `turn.end`, because the turn did not end.
    let events = log.committed(2);
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    assert_eq!(kinds, ["turn.start", MESSAGE_DELTA], "{kinds:?}");
}

#[tokio::test]
async fn a_turn_that_settles_aborted_answers_the_runner_with_aborted() {
    let dir = TempDir::new("rpc-aborted");
    let log = Log::new(dir);
    let script = log.dir.write(
        "aborted.sh",
        "#!/bin/sh\n\
         IFS= read -r line\n\
         printf '{\"type\":\"agent_start\"}\n'\n\
         printf '{\"type\":\"agent_settled\",\"aborted\":true}\n'\n\
         cat > /dev/null\n",
    );
    let _ = std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(&script);
    let (agent, _exit) = Agent::launch(command, &log.session.id).expect("the fake agent starts");
    let mut records = agent.records();
    let _ = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await;
    let mut mapping = Mapping::new(&log.session.id);
    let ended = pump(
        &mut records,
        &mut mapping,
        &log.writer,
        &log.store(),
        &agent,
    )
    .await
    .expect("the settle answers");
    assert_eq!(ended, holosmith::backend::TurnEnd::Aborted);
    // An aborted turn still ends: the boundary pair is `pump`'s, and the runner's `abort_completed` row is what
    // the transcript draws the turn's end from.
    let events = log.committed(2);
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    assert_eq!(kinds, ["turn.start", "turn.end"], "{kinds:?}");
}

/// The three things a turn can be handed: a record, the end of the process, and the same end again.
///
/// The last arm is the one the design needs and the old broadcast could not give: the agent's registry entry keeps
/// the publication slot alive for as long as the session exists, so "the process is gone" has to be the reader's
/// own last word rather than the channel closing. A pump that asked again after a `Closed` must not wait for a
/// write that will never come.
#[tokio::test]
async fn the_records_end_with_the_process_and_answer_closed_from_then_on() {
    let dir = TempDir::new("rpc-closed");
    let script = dir.write(
        "one.sh",
        "#!/bin/sh\nIFS= read -r line\nprintf '{\"type\":\"agent_start\"}\n'\n",
    );
    let _ = std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(&script);
    let (agent, _exit) = Agent::launch(command, "0123456789abcdef").expect("the agent starts");
    let mut records = agent.records();
    // The record is published on the same pipe, so the prompt is what starts the script's body.
    let _ = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await;

    match records.recv().await {
        Received::Record(record) => assert_eq!(record["type"], "agent_start"),
        other => panic!("the script printed one record: {other:?}"),
    }
    assert_eq!(records.recv().await, Received::Closed);
    // And it keeps answering: the agent value is still alive in the registry, so a channel that only closed once
    // would leave the next call waiting on nothing.
    assert_eq!(records.recv().await, Received::Closed);
}

/// Every tool call `fixture` finishes, as the mapping hands them over — the whole record, which is what the
/// pump stores. The frames a direct replay can build are `mapped`'s; the images are the part that only the
/// store-less half sees, because a frame with no store names no file.
fn tool_ends(fixture: &str) -> Vec<ToolEnd> {
    let mut mapping = Mapping::new("0123456789abcdef");
    records(fixture)
        .into_iter()
        .filter_map(|record| match mapping.map(&record) {
            Mapped::Tool(result) => Some(result),
            _ => None,
        })
        .collect()
}

#[test]
fn the_recorded_image_turn_names_the_picture_and_the_text_still_says_what_it_was() {
    // V8's first case, from a real turn: `read` on a four-pixel PNG, whose result is a text block and an
    // `image` block. The mapping must see the image (the pump is what stores it, below) and the text must keep
    // a trace of it, so a transcript with no store still says something was there.
    let ends = tool_ends(IMAGE);
    assert_eq!(ends.len(), 1, "one call finished in this turn");
    let end = &ends[0];
    assert_eq!(end.name, "read");
    assert_eq!(
        end.images.len(),
        1,
        "the result carried one image block: {:?}",
        end.images
    );
    let picture = &end.images[0];
    assert_eq!(picture.mime, "image/png");
    assert_eq!(
        picture.index, 1,
        "the block's own index in `result.content`: the text block is 0"
    );
    assert!(
        picture.data.len() > 80,
        "the base64 body is the picture, not a placeholder: {} characters",
        picture.data.len()
    );
    assert_eq!(
        end.output, "Read image file [image/png][image]",
        "the text keeps the tool's own sentence and a bracket for the block it cannot print"
    );

    // And the replay's frame, which has no store to name a file with: no `images` key rather than a reference to
    // nothing. The pump's version of this frame is asserted below.
    let (frames, settled, skipped, unknown) = mapped(IMAGE);
    print_frames("image", &frames);
    assert_eq!(settled, vec![false]);
    assert_eq!(unknown, 0, "every record in this turn is one pi documents");
    let end = of_kind(&frames, TOOL_END)[0];
    assert_eq!(end.payload["name"], "read");
    assert!(
        end.payload.get("images").is_none(),
        "a frame built without a store must not claim a file: {}",
        end.payload
    );
    assert!(
        skipped > 0,
        "the turn declined some of pi's bookkeeping, and that is the design"
    );
}

#[test]
fn the_recorded_extension_turn_cancels_the_dialog_and_says_which_one_asked() {
    // V8's third case, from a real turn: a project-local extension calls `ctx.ui.select`, which in RPC mode
    // emits an `extension_ui_request` and **blocks the run** until the client answers. The mapping's answer is a
    // cancel, and the frame it leaves is what makes an extension's odd behaviour explicable.
    let (frames, settled, skipped, unknown) = mapped(EXTENSION_UI);
    print_frames("extension ui", &frames);
    assert_eq!(settled, vec![false], "the turn settled: nothing hangs");
    assert_eq!(unknown, 0, "both requests are ones pi documents");
    let errors = of_kind(&frames, TURN_ERROR);
    assert_eq!(
        errors.len(),
        1,
        "one dialog, one frame — the notify asks for nothing and answers nothing"
    );
    let frame = errors[0];
    assert_eq!(frame.payload["method"], "select");
    let reason = frame.payload["reason"].as_str().expect("a reason");
    assert!(
        reason.contains("select") && reason.contains("cancelled"),
        "the sentence names the method and what was done: {reason}"
    );
    assert_eq!(
        frame.payload.as_object().expect("an object").len(),
        2,
        "two keys, and no message id: it is not a transcript block of a message"
    );
    // Declined: the fixture's `response`, `agent_start`, the notify, pi's own two boundaries, `agent_end`, and
    // the two ends of the assistant's text block. Nothing of an unknown type — which is the difference between
    // "documented with no frame" and "a release grew something this build has never seen".
    assert_eq!(
        skipped, 8,
        "the documented records with no frame, counted once each"
    );

    // The fixture also carries the proof that the cancel *reached* the extension: pi's `notify`, emitted after
    // the dialog returned, says what the dialog answered. A build that answered nothing would not have this
    // line at all — it would be sitting in `select`.
    let notify = records(EXTENSION_UI)
        .into_iter()
        .find(|record| record.get("method").and_then(Value::as_str) == Some("notify"))
        .expect("the fixture's notify");
    let message = notify["message"].as_str().expect("a message");
    assert!(
        message.contains("cancelled"),
        "the extension's own words about what it got: {message}"
    );
}

#[tokio::test]
async fn the_pump_stores_the_images_of_a_tool_result_and_names_them_in_the_frame() {
    // The served half of V8's first case: the pump writes the picture beside the tool's text and the frame names
    // it, so a phone draws an image instead of a paragraph about one.
    let dir = TempDir::new("rpc-image");
    let log = Log::new(dir);
    let command = fake_agent(&log.dir, IMAGE);
    let (agent, _exit) = Agent::launch(command, &log.session.id).expect("the fake agent starts");
    let mut records = agent.records();
    let _ = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await;
    let mut mapping = Mapping::new(&log.session.id);
    let ended = pump(
        &mut records,
        &mut mapping,
        &log.writer,
        &log.store(),
        &agent,
    )
    .await
    .expect("the fixture settles the turn");
    assert_eq!(ended, holosmith::backend::TurnEnd::Settled);

    // The writer commits every 25 ms, so "the pump returned" is not "the rows are there": wait for the frame
    // this test is about, then read the log.
    let deadline = Instant::now() + PATIENCE;
    let events = loop {
        let events = log
            .database
            .lock()
            .expect("the lock")
            .events()
            .read_since(&log.session.id, 0, 10_000)
            .expect("the log reads");
        if events.iter().any(|event| event.kind == "turn.end") {
            break events;
        }
        assert!(Instant::now() < deadline, "the turn never ended");
        std::thread::sleep(Duration::from_millis(10));
    };
    let payload_of = |kind: &str| -> Value {
        let event = events
            .iter()
            .find(|event| event.kind == kind)
            .unwrap_or_else(|| panic!("no {kind} in the log: {events:?}"));
        serde_json::from_str(&event.payload).expect("a payload is JSON")
    };
    let tool_end = payload_of(TOOL_END);
    let images = tool_end["images"]
        .as_array()
        .unwrap_or_else(|| panic!("the frame names its images: {tool_end}"));
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["index"], 1);
    assert_eq!(images[0]["mime"], "image/png");
    eprintln!("tool.end in the log: {tool_end}");
    let reference = images[0]["output_ref"].as_str().expect("a reference");
    assert_eq!(
        reference,
        format!(
            "/api/sessions/{}/tool-image/{}/1",
            log.session.id, "call_a86a8cbeb3f9441ba986c633"
        ),
        "the reference is the route plus the pair the frame gave"
    );
    // No byte of the picture is in the payload: that is the whole reason it is a file.
    let payload = serde_json::to_string(&tool_end).expect("serialised");
    assert!(
        !payload.contains("iVBORw0KGgo"),
        "a PNG's base64 header reached the log: {payload}"
    );

    // The file is the picture, and the store serves it as one: the four-pixel PNG of the turn's own scratch
    // directory, byte for byte.
    let store = log.store();
    let (bytes, mime) = store
        .read_image(&log.session.id, "call_a86a8cbeb3f9441ba986c633", 1)
        .expect("the image is on disk");
    assert_eq!(mime, "image/png");
    assert_eq!(
        &bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        "the file is the picture, not the base64 of it"
    );
    // The picture is the one the fixture's prompt told pi to read: a 4x4 PNG with a red left half and a blue
    // right half, 76 bytes, so "the file is the image" is a claim with a number and a header behind it.
    assert_eq!(bytes.len(), 76, "the stored picture is the one pi read");
}

#[tokio::test]
async fn the_pump_answers_a_dialog_with_a_cancel_and_records_that_it_did() {
    // V8's third case at the served path. The mapping decides (`Mapped::Cancel`), and the **pump** writes the
    // answer, because pi blocks on it: a frame without the write would be a log claiming a cancel that never
    // reached the extension, and the turn would hang.
    let dir = TempDir::new("rpc-dialog");
    let log = Log::new(dir);
    let (command, captured) = fake_agent_recording_stdin(&log.dir, EXTENSION_UI);
    let (agent, _exit) = Agent::launch(command, &log.session.id).expect("the fake agent starts");
    // Named `stream`, not `records`: `records` is this file's fixture parser, and the two would shadow.
    let mut stream = agent.records();
    let _ = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await;
    let mut mapping = Mapping::new(&log.session.id);
    let ended = pump(&mut stream, &mut mapping, &log.writer, &log.store(), &agent)
        .await
        .expect("the dialog is answered and the turn settles");
    assert_eq!(
        ended,
        holosmith::backend::TurnEnd::Settled,
        "a session that ignored the dialog would still be waiting on it"
    );

    // What the agent was handed. The fake agent records only what arrives *after* the prompt, so this file is
    // the answers and nothing else — and the notify in the fixture is what a second line would be.
    let deadline = Instant::now() + PATIENCE;
    let written = loop {
        let text = std::fs::read_to_string(&captured).unwrap_or_default();
        if text.contains('\n') {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "the pump wrote nothing to the agent's stdin: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "one dialog is one answer, and a fire-and-forget request is answered with nothing: {written}"
    );
    eprintln!("stdin the pump wrote: {}", lines[0]);
    let answer: Value = serde_json::from_str(lines[0]).expect("a JSONL line");
    let asked = records(EXTENSION_UI)
        .into_iter()
        .find(|record| record.get("method").and_then(Value::as_str) == Some("select"))
        .expect("the fixture's dialog");
    assert_eq!(
        answer,
        json!({"type": "extension_ui_response", "id": asked["id"], "cancelled": true}),
        "the response repeats the request's own id, which is what pi matches it by"
    );

    // And the frame that says it happened, once, in the turn's own log.
    let deadline = Instant::now() + PATIENCE;
    let events = loop {
        let events = log
            .database
            .lock()
            .expect("the lock")
            .events()
            .read_since(&log.session.id, 0, 10_000)
            .expect("the log reads");
        if events.iter().any(|event| event.kind == "turn.end") {
            break events;
        }
        assert!(Instant::now() < deadline, "the turn never ended");
        std::thread::sleep(Duration::from_millis(10));
    };
    let errors: Vec<Value> = events
        .iter()
        .filter(|event| event.kind == TURN_ERROR)
        .map(|event| serde_json::from_str(&event.payload).expect("a payload is JSON"))
        .collect();
    eprintln!("the frame it left: {}", errors[0]);
    assert_eq!(errors.len(), 1, "{events:?}");
    assert_eq!(errors[0]["method"], "select");
    assert!(
        errors[0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("cancelled")),
        "{}",
        errors[0]
    );
}
