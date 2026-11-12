//! `holosmith container fake-pi`, measured as a process (spec item 10 step 2).
//!
//! The smoke run needs a container's `pi` that replays a recorded turn, and the only honest place to measure that is
//! a real process over two real pipes: [`holosmith::agent::launch`] is the *shipped* reader — it splits records on LF
//! only, routes `response` records by command id, publishes the rest and sends the exit after the last record — so a
//! fake that danied a wire it shared with pi would fail here rather than at a promotion.
//!
//! `CARGO_BIN_EXE_holosmith` is the binary these tests spawn, which is the same file `cargo test` compiled: there is no
//! daemon, no image and no network in any of them.

use std::process::Command;
use std::time::Duration;

use holosmith::agent::{self, Ended, Received};
use holosmith::rpc::{Mapped, Mapping};
use serde_json::{Value, json};

/// One command that speaks pi's own args, exactly as [`holosmith::container::SessionContainer::run_args`] does.
fn pi() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_holosmith"));
    command.args([
        "container",
        "fake-pi",
        "--mode",
        "rpc",
        "--no-session",
        "--approve",
        "--model",
        "claude-sonnet-4.5",
        "--thinking",
        "high",
    ]);
    command
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime is buildable")
}

/// How long the fake may take to answer anything. Generous, because the first call in a debug build pays the dynamic
/// loader; a bound rather than a sleep, because a hang must fail the test rather than wedge the suite.
const BOUND: Duration = Duration::from_secs(30);

/// The image's `pi` answers the pinned version, which is what keeps `agent::version_check` measured in a smoke run.
#[test]
fn version_is_the_pinned_one() {
    let output = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(["container", "fake-pi", "--version"])
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "the fake refused --version");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        holosmith::pins::pi_version(),
        "the image's pi must answer the pin, or the launch records a version the binary is not built for"
    );
}

/// The handshake, the prompt answer and the exit — through the shipped launch, with no daemon.
///
/// Three protocol facts in one test, because they are one conversation: `get_state` is answered with the id that was
/// sent (the ready handshake), `prompt` is answered with **its** id (never the recording's `req-1`), and closing stdin
/// exits 0 — which is what a delete does, and what the exit classifier reads as an orderly stop.
#[test]
fn the_launch_handshakes_prompts_and_exits_cleanly_on_a_closed_stdin() {
    let runtime = runtime();
    runtime.block_on(async {
        let (agent, exit) = agent::Agent::launch(pi(), "0123456789abcdef").expect("the fake starts");
        let reply = agent
            .ready()
            .await
            .expect("the ready handshake is answered");
        assert!(reply.success);
        assert_eq!(
            reply.data.and_then(|data| data.get("isStreaming").cloned()),
            Some(json!(false)),
            "a get_state answer carries pi's state, which is what the handshake reads"
        );

        // Subscribe **before** the command: pi starts printing as soon as it accepts, and a record printed between the
        // write and the subscription is a hole in the log (ADR-0040).
        let mut records = agent.records();
        let reply = agent
            .request(
                "turn-1",
                json!({"id": "turn-1", "type": "prompt", "message": "replay the recorded turn"}),
                BOUND,
            )
            .await
            .expect("the prompt is answered");
        assert!(reply.success);
        assert_eq!(
            reply
                .data
                .and_then(|data| data.get("disposition").and_then(Value::as_str).map(str::to_string)),
            Some("started".to_string()),
            "the backend logs the disposition, and pi documents `started` for a prompt that begins a run"
        );

        // The recorded turn, record by record, until pi says it has no work left.
        let mut seen: Vec<Value> = Vec::new();
        let mut settled = false;
        loop {
            let next = tokio::time::timeout(BOUND, records.recv())
                .await
                .expect("the fake keeps printing");
            match next {
                Received::Record(record) => {
                    settled = record.get("type").and_then(Value::as_str) == Some("agent_settled");
                    seen.push(record);
                    if settled {
                        break;
                    }
                }
                Received::Closed => break,
            }
        }
        assert!(settled, "the replay never reached `agent_settled`: {} records", seen.len());
        assert!(
            !seen.iter().any(|record| record.get("id").and_then(Value::as_str) == Some("req-1")),
            "the recording's own command id was published as an answer"
        );

        // The writer half's three facts, in the recording's own vocabulary.
        let count = |kind: &str| {
            seen.iter()
                .filter(|record| record.get("type").and_then(Value::as_str) == Some(kind))
                .count()
        };
        assert!(count("message_update") > 0, "no deltas arrived");
        assert_eq!(count("tool_execution_start"), 1, "one tool call starts");
        assert_eq!(count("tool_execution_end"), 1, "and ends");
        let usage = seen
            .iter()
            .filter_map(|record| record.get("message"))
            .filter_map(|message| message.get("usage"))
            .filter_map(|usage| usage.get("totalTokens").and_then(Value::as_u64))
            .max()
            .unwrap_or(0);
        assert!(usage > 0, "the recording reports no usage");

        // Closing stdin is an orderly end: `serve` does exactly this on a delete, and a non-zero code would be
        // classified as a failed agent.
        drop(agent);
        let Ended { code, stderr } = tokio::time::timeout(BOUND, exit)
            .await
            .expect("the exit arrives after the last record")
            .expect("the exit is reported");
        assert_eq!(
            code, 0,
            "closing stdin must end the fake with 0, and it printed: {stderr}"
        );
    });
}

/// The frames the shipped mapping makes of the replayed records: a delta, a tool pair with one id, and usage.
///
/// This is the arm the acceptance names — "including deltas, a tool call and usage" — put through
/// [`holosmith::rpc::Mapping`], the same function `pump` uses in a served turn. It is the one test here that would
/// catch a replay whose records were well-formed but out of the order the mapping needs.
#[test]
fn the_replayed_records_map_to_a_delta_a_tool_pair_and_a_usage() {
    let output = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(["container", "fake-pi"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary runs");
    {
        use std::io::Write as _;
        output
            .stdin
            .as_ref()
            .expect("a piped stdin")
            .write_all(b"{\"id\":\"t-1\",\"type\":\"prompt\",\"message\":\"hi\"}\n")
            .expect("the command is written");
    }
    let output = output.wait_with_output().expect("the fake exits");
    assert!(output.status.success(), "the fake refused the prompt");
    let stdout = String::from_utf8(output.stdout).expect("the replay writes UTF-8");

    let mut mapping = Mapping::new("0123456789abcdef");
    let mut frames: Vec<(String, Value)> = Vec::new();
    let mut settled = false;
    for line in stdout.lines() {
        let record: Value = serde_json::from_str(line).expect("every line is a record");
        match mapping.map(&record) {
            Mapped::Frame { kind, payload } => frames.push((
                kind.to_string(),
                serde_json::from_str(&payload).expect("a frame's payload is JSON"),
            )),
            Mapped::Settled { aborted } => {
                assert!(!aborted, "the replay was not aborted");
                settled = true;
            }
            // A tool's result with no data root: the mapping answers the frame's previewed shape, which is what this
            // test needs (the storage half is `tests/rpc.rs`'s).
            other => {
                if let Some((kind, payload)) = other.into_frame() {
                    frames.push((
                        kind.to_string(),
                        serde_json::from_str(&payload).expect("a frame's payload is JSON"),
                    ));
                }
            }
        }
    }
    assert!(settled, "the mapping never saw `agent_settled`");

    let kinds: Vec<&str> = frames.iter().map(|(kind, _)| kind.as_str()).collect();
    assert!(
        kinds.contains(&"message.delta"),
        "no delta frame: {kinds:?}"
    );
    let starts: Vec<&Value> = frames
        .iter()
        .filter(|(kind, _)| kind == "tool.start")
        .map(|(_, payload)| payload)
        .collect();
    let ends: Vec<&Value> = frames
        .iter()
        .filter(|(kind, _)| kind == "tool.end")
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(starts.len(), 1, "one tool call: {kinds:?}");
    assert_eq!(ends.len(), 1, "one tool result: {kinds:?}");
    assert_eq!(
        starts[0]["tool_id"], ends[0]["tool_id"],
        "the pair's id is what makes one block, not two"
    );
    let usage = frames
        .iter()
        .filter(|(kind, _)| kind == "message.end")
        .filter_map(|(_, payload)| payload.get("usage"))
        .next()
        .expect("a message.end carries usage");
    assert!(
        usage["in"].as_u64().unwrap_or(0) > 0,
        "the usage frame counts what the turn read: {usage}"
    );
}

/// A prompt with no command ahead of it is not a turn: nothing replays, and the exit is still 0 (an empty stdin is an
/// orderly end, which `the_launch_handshakes…` closes the loop on).
#[test]
fn an_unparseable_command_is_skipped_rather_than_fatal() {
    use std::io::Write as _;
    let mut child = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(["container", "fake-pi"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary runs");
    child
        .stdin
        .as_mut()
        .expect("a piped stdin")
        .write_all(b"not json\n{\"type\":\"get_state\"}\n{\"id\":\"ok\",\"type\":\"get_state\"}\n")
        .expect("the lines are written");
    let output = child.wait_with_output().expect("the fake exits");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "only the command with an id is answered, and a line that is not JSON does not kill the process: {stdout}"
    );
    assert!(
        matches!(
            serde_json::from_str::<Value>(stdout.lines().next().unwrap_or_default()),
            Ok(ref value) if value["id"] == "ok"
        ),
        "the answer echoes the id of the command it was given: {stdout}"
    );
}
