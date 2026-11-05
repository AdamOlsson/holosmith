//! pi's records become the transcript's frames (HS-032).
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

use rusqlite::Connection;
use serde_json::{Value, json};

use holosmith::agent::{Agent, Received};
use holosmith::db::{Database, OpenOptions};
use holosmith::events::{self, Event, MESSAGE_DELTA, MESSAGE_END, WriterOptions};
use holosmith::rpc::{Mapped, Mapping, pump};
use holosmith::sessions::{NewSession, Session};

mod support;
use support::TempDir;

/// The turn that answered with text and nothing else.
const TEXT: &str = include_str!("fixtures/pi-turn-text.jsonl");

/// The turn that thought before it answered.
const THINKING: &str = include_str!("fixtures/pi-turn-thinking.jsonl");

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
fn mapped(fixture: &str) -> (Vec<Frame>, Vec<bool>, u64) {
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
            Mapped::Nothing => {}
        }
    }
    (frames, settled, mapping.skipped())
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
    let (frames, settled, skipped) = mapped(TEXT);
    print_frames("text", &frames);
    let kinds: Vec<&str> = frames.iter().map(|frame| frame.kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["turn.start", MESSAGE_DELTA, MESSAGE_END, "turn.end",],
        "one text block, its end, and the turn's boundaries: {kinds:?}"
    );
    assert_eq!(settled, vec![false], "the agent settled, and did not abort");
    assert!(
        skipped > 0,
        "the fixture carries pi's system prompt, its user message and its own bookkeeping, and none of \
         them is a frame"
    );

    assert_eq!(text_of(&frames, "text"), "hello world");
    assert_eq!(text_of(&frames, "thinking"), "");

    let end = of_kind(&frames, MESSAGE_END)[0];
    assert_eq!(
        end.payload["message_id"], frames[1].payload["message_id"],
        "the message's deltas and its end share the id the mapping minted"
    );
    assert_eq!(end.payload["usage"], json!({"in": 991, "out": 4}));
    assert_eq!(end.payload["stop_reason"], "stop");
    assert_eq!(
        end.payload["tool_calls"],
        json!([]),
        "this turn called no tool"
    );
}

#[test]
fn the_recorded_thinking_turn_streams_thinking_before_text() {
    let (frames, settled, _) = mapped(THINKING);
    print_frames("thinking", &frames);
    assert_eq!(settled, vec![false]);

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
fn a_fixture_whose_frames_drifted_fails_this_guard() {
    // The guard that makes the two replays above meaningful: the fixtures are committed verbatim, and this
    // test fails if either stops carrying the records the mapping's arms are proven against. It is cheap and
    // it is the reason a fixture refresh is a deliberate act rather than an accident.
    for (name, fixture) in [("text", TEXT), ("thinking", THINKING)] {
        let (frames, settled, _) = mapped(fixture);
        assert_eq!(settled.len(), 1, "{name}: one `agent_settled`");
        assert_eq!(
            of_kind(&frames, MESSAGE_END).len(),
            1,
            "{name}: one assistant message"
        );
        assert_eq!(of_kind(&frames, "turn.start").len(), 1, "{name}");
        assert!(!text_of(&frames, "text").is_empty(), "{name}: text");
    }
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
    let ended = pump(&mut records, &mut mapping, &log.writer)
        .await
        .expect("the fixture settles the turn");
    assert_eq!(ended, holosmith::backend::TurnEnd::Settled);
    // Two records with no frame: `agent_start` and `agent_end`. A direct replay of the fixture counts three,
    // because it also maps the fixture's `response` line — the record that answers the prompt, which the
    // reader routes to its caller and never publishes to a turn.
    assert_eq!(mapping.skipped(), 2, "agent_start and agent_end");
    assert_eq!(
        mapped(TEXT).2,
        3,
        "the replay also maps the prompt's `response`"
    );

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
    assert_eq!(payload(2)["usage"], json!({"in": 991, "out": 4}));
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
    pump(&mut records, &mut mapping, &log.writer)
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
    let script = dir.write(
        "dies.sh",
        "#!/bin/sh\n\
         IFS= read -r line\n\
         printf '{\"type\":\"turn_start\"}\\n'\n\
         printf '{\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"content\":[]}}\\n'\n\
         printf '{\"type\":\"message_update\",\"assistantMessageEvent\":\
         {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"half\"}}\\n'\n",
    );
    let _ = std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(&script);

    let database = Arc::new(Mutex::new(
        Database::open(&dir.database(), OpenOptions::default(), NOW).expect("a database opens"),
    ));
    let (writer, _stream) = events::spawn(Arc::clone(&database), WriterOptions::default());
    let session = database
        .lock()
        .expect("the lock")
        .sessions()
        .create(
            &NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: Some("rpc-dies".to_string()),
                base_commit: None,
            },
            NOW,
        )
        .expect("a session is created");

    let (agent, _exit) = Agent::launch(command, &session.id).expect("the fake agent starts");
    let mut records = agent.records();
    // The `prompt` write races the process's exit, and either arm is the same answer for this test: the run
    // is over before it settled.
    let _ = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await;
    let mut mapping = Mapping::new(&session.id);
    let ended = pump(&mut records, &mut mapping, &writer).await;
    let error = ended.expect_err("an agent that never settles fails its turn");
    assert!(
        error.to_string().contains("settled"),
        "the reason names the missing settle: {error}"
    );

    // The delta it did print is still in the log: a failed turn is not a silent one.
    let deadline = Instant::now() + PATIENCE;
    loop {
        let count: i64 = Connection::open(dir.database())
            .expect("a second connection opens")
            .query_row(
                "SELECT count(*) FROM events WHERE session_id = ?1 AND type = ?2",
                [&session.id, MESSAGE_DELTA],
                |row| row.get(0),
            )
            .expect("the count answers");
        if count == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "the delta reached the log");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test]
async fn a_turn_that_settles_aborted_answers_the_runner_with_aborted() {
    let dir = TempDir::new("rpc-aborted");
    let script = dir.write(
        "aborted.sh",
        "#!/bin/sh\n\
         IFS= read -r line\n\
         printf '{\"type\":\"agent_start\"}\\n'\n\
         printf '{\"type\":\"agent_settled\",\"aborted\":true}\\n'\n\
         cat > /dev/null\n",
    );
    let _ = std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let database = Arc::new(Mutex::new(
        Database::open(&dir.database(), OpenOptions::default(), NOW).expect("a database opens"),
    ));
    let (writer, _stream) = events::spawn(Arc::clone(&database), WriterOptions::default());
    let session = database
        .lock()
        .expect("the lock")
        .sessions()
        .create(
            &NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: Some("rpc-aborted".to_string()),
                base_commit: None,
            },
            NOW,
        )
        .expect("a session is created");

    let mut command = std::process::Command::new("/bin/sh");
    command.arg(&script);
    let (agent, _exit) = Agent::launch(command, &session.id).expect("the fake agent starts");
    let mut records = agent.records();
    let _ = agent
        .request(
            "prompt-1",
            json!({"id": "prompt-1", "type": "prompt", "message": "hello"}),
            Duration::from_secs(5),
        )
        .await;
    let mut mapping = Mapping::new(&session.id);
    let ended = pump(&mut records, &mut mapping, &writer)
        .await
        .expect("the settle answers");
    assert_eq!(ended, holosmith::backend::TurnEnd::Aborted);
}

/// The unreachable arm, kept honest: a subscription whose sender has gone reports `Closed`.
#[tokio::test]
async fn a_closed_agent_reports_a_closed_stream() {
    // Nothing is launched: `Agent::launch` drops the sender with the process, so a subscription with no writer
    // left answers `Closed` rather than blocking. `Received` is the mapping's only input, so its three arms are
    // worth one assertion each.
    let dir = TempDir::new("rpc-closed");
    let script = dir.write("quiet.sh", "#!/bin/sh\ncat > /dev/null\n");
    let _ = std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(&script);
    let (agent, _exit) = Agent::launch(command, "0123456789abcdef").expect("the agent starts");
    let mut records = agent.records();
    drop(agent);
    assert_eq!(records.recv().await, Received::Closed);
}
